//! Code shared by trait and projection goals for candidate assembly.

use super::search_graph::OverflowHandler;
use super::{EvalCtxt, SolverMode};
use crate::traits::coherence;
use rustc_data_structures::fx::FxIndexSet;
use rustc_hir::def_id::DefId;
use rustc_infer::traits::query::NoSolution;
use rustc_infer::traits::util::elaborate;
use rustc_infer::traits::Reveal;
use rustc_middle::traits::solve::inspect::CandidateKind;
use rustc_middle::traits::solve::{CanonicalResponse, Certainty, Goal, MaybeCause, QueryResult};
use rustc_middle::ty::fast_reject::TreatProjections;
use rustc_middle::ty::TypeFoldable;
use rustc_middle::ty::{self, Ty, TyCtxt};
use std::fmt::Debug;

pub(super) mod structural_traits;

/// A candidate is a possible way to prove a goal.
///
/// It consists of both the `source`, which describes how that goal would be proven,
/// and the `result` when using the given `source`.
#[derive(Debug, Clone)]
pub(super) struct Candidate<'tcx> {
    pub(super) source: CandidateSource,
    pub(super) result: CanonicalResponse<'tcx>,
}

/// Possible ways the given goal can be proven.
#[derive(Debug, Clone, Copy)]
pub(super) enum CandidateSource {
    /// A user written impl.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// fn main() {
    ///     let x: Vec<u32> = Vec::new();
    ///     // This uses the impl from the standard library to prove `Vec<T>: Clone`.
    ///     let y = x.clone();
    /// }
    /// ```
    Impl(DefId),
    /// A builtin impl generated by the compiler. When adding a new special
    /// trait, try to use actual impls whenever possible. Builtin impls should
    /// only be used in cases where the impl cannot be manually be written.
    ///
    /// Notable examples are auto traits, `Sized`, and `DiscriminantKind`.
    /// For a list of all traits with builtin impls, check out the
    /// [`EvalCtxt::assemble_builtin_impl_candidates`] method. Not
    BuiltinImpl,
    /// An assumption from the environment.
    ///
    /// More precisely we've used the `n-th` assumption in the `param_env`.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// fn is_clone<T: Clone>(x: T) -> (T, T) {
    ///     // This uses the assumption `T: Clone` from the `where`-bounds
    ///     // to prove `T: Clone`.
    ///     (x.clone(), x)
    /// }
    /// ```
    ParamEnv(usize),
    /// If the self type is an alias type, e.g. an opaque type or a projection,
    /// we know the bounds on that alias to hold even without knowing its concrete
    /// underlying type.
    ///
    /// More precisely this candidate is using the `n-th` bound in the `item_bounds` of
    /// the self type.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// trait Trait {
    ///     type Assoc: Clone;
    /// }
    ///
    /// fn foo<T: Trait>(x: <T as Trait>::Assoc) {
    ///     // We prove `<T as Trait>::Assoc` by looking at the bounds on `Assoc` in
    ///     // in the trait definition.
    ///     let _y = x.clone();
    /// }
    /// ```
    AliasBound,
}

/// Methods used to assemble candidates for either trait or projection goals.
pub(super) trait GoalKind<'tcx>:
    TypeFoldable<TyCtxt<'tcx>> + Copy + Eq + std::fmt::Display
{
    fn self_ty(self) -> Ty<'tcx>;

    fn trait_ref(self, tcx: TyCtxt<'tcx>) -> ty::TraitRef<'tcx>;

    fn with_self_ty(self, tcx: TyCtxt<'tcx>, self_ty: Ty<'tcx>) -> Self;

    fn trait_def_id(self, tcx: TyCtxt<'tcx>) -> DefId;

    // Try equating an assumption predicate against a goal's predicate. If it
    // holds, then execute the `then` callback, which should do any additional
    // work, then produce a response (typically by executing
    // [`EvalCtxt::evaluate_added_goals_and_make_canonical_response`]).
    fn probe_and_match_goal_against_assumption(
        ecx: &mut EvalCtxt<'_, 'tcx>,
        goal: Goal<'tcx, Self>,
        assumption: ty::Binder<'tcx, ty::ClauseKind<'tcx>>,
        then: impl FnOnce(&mut EvalCtxt<'_, 'tcx>) -> QueryResult<'tcx>,
    ) -> QueryResult<'tcx>;

    // Consider a clause, which consists of a "assumption" and some "requirements",
    // to satisfy a goal. If the requirements hold, then attempt to satisfy our
    // goal by equating it with the assumption.
    fn consider_implied_clause(
        ecx: &mut EvalCtxt<'_, 'tcx>,
        goal: Goal<'tcx, Self>,
        assumption: ty::Binder<'tcx, ty::ClauseKind<'tcx>>,
        requirements: impl IntoIterator<Item = Goal<'tcx, ty::Predicate<'tcx>>>,
    ) -> QueryResult<'tcx> {
        Self::probe_and_match_goal_against_assumption(ecx, goal, assumption, |ecx| {
            ecx.add_goals(requirements);
            ecx.evaluate_added_goals_and_make_canonical_response(Certainty::Yes)
        })
    }

    /// Consider a bound originating from the item bounds of an alias. For this we
    /// require that the well-formed requirements of the self type of the goal
    /// are "satisfied from the param-env".
    /// See [`EvalCtxt::validate_alias_bound_self_from_param_env`].
    fn consider_alias_bound_candidate(
        ecx: &mut EvalCtxt<'_, 'tcx>,
        goal: Goal<'tcx, Self>,
        assumption: ty::Binder<'tcx, ty::ClauseKind<'tcx>>,
    ) -> QueryResult<'tcx> {
        Self::probe_and_match_goal_against_assumption(ecx, goal, assumption, |ecx| {
            ecx.validate_alias_bound_self_from_param_env(goal)
        })
    }

    // Consider a clause specifically for a `dyn Trait` self type. This requires
    // additionally checking all of the supertraits and object bounds to hold,
    // since they're not implied by the well-formedness of the object type.
    fn consider_object_bound_candidate(
        ecx: &mut EvalCtxt<'_, 'tcx>,
        goal: Goal<'tcx, Self>,
        assumption: ty::Binder<'tcx, ty::ClauseKind<'tcx>>,
    ) -> QueryResult<'tcx> {
        Self::probe_and_match_goal_against_assumption(ecx, goal, assumption, |ecx| {
            let tcx = ecx.tcx();
            let ty::Dynamic(bounds, _, _) = *goal.predicate.self_ty().kind() else {
                    bug!("expected object type in `consider_object_bound_candidate`");
                };
            ecx.add_goals(
                structural_traits::predicates_for_object_candidate(
                    &ecx,
                    goal.param_env,
                    goal.predicate.trait_ref(tcx),
                    bounds,
                )
                .into_iter()
                .map(|pred| goal.with(tcx, pred)),
            );
            ecx.evaluate_added_goals_and_make_canonical_response(Certainty::Yes)
        })
    }

    fn consider_impl_candidate(
        ecx: &mut EvalCtxt<'_, 'tcx>,
        goal: Goal<'tcx, Self>,
        impl_def_id: DefId,
    ) -> QueryResult<'tcx>;

    // A type implements an `auto trait` if its components do as well. These components
    // are given by built-in rules from [`instantiate_constituent_tys_for_auto_trait`].
    fn consider_auto_trait_candidate(
        ecx: &mut EvalCtxt<'_, 'tcx>,
        goal: Goal<'tcx, Self>,
    ) -> QueryResult<'tcx>;

    // A trait alias holds if the RHS traits and `where` clauses hold.
    fn consider_trait_alias_candidate(
        ecx: &mut EvalCtxt<'_, 'tcx>,
        goal: Goal<'tcx, Self>,
    ) -> QueryResult<'tcx>;

    // A type is `Copy` or `Clone` if its components are `Sized`. These components
    // are given by built-in rules from [`instantiate_constituent_tys_for_sized_trait`].
    fn consider_builtin_sized_candidate(
        ecx: &mut EvalCtxt<'_, 'tcx>,
        goal: Goal<'tcx, Self>,
    ) -> QueryResult<'tcx>;

    // A type is `Copy` or `Clone` if its components are `Copy` or `Clone`. These
    // components are given by built-in rules from [`instantiate_constituent_tys_for_copy_clone_trait`].
    fn consider_builtin_copy_clone_candidate(
        ecx: &mut EvalCtxt<'_, 'tcx>,
        goal: Goal<'tcx, Self>,
    ) -> QueryResult<'tcx>;

    // A type is `PointerLike` if we can compute its layout, and that layout
    // matches the layout of `usize`.
    fn consider_builtin_pointer_like_candidate(
        ecx: &mut EvalCtxt<'_, 'tcx>,
        goal: Goal<'tcx, Self>,
    ) -> QueryResult<'tcx>;

    // A type is a `FnPtr` if it is of `FnPtr` type.
    fn consider_builtin_fn_ptr_trait_candidate(
        ecx: &mut EvalCtxt<'_, 'tcx>,
        goal: Goal<'tcx, Self>,
    ) -> QueryResult<'tcx>;

    // A callable type (a closure, fn def, or fn ptr) is known to implement the `Fn<A>`
    // family of traits where `A` is given by the signature of the type.
    fn consider_builtin_fn_trait_candidates(
        ecx: &mut EvalCtxt<'_, 'tcx>,
        goal: Goal<'tcx, Self>,
        kind: ty::ClosureKind,
    ) -> QueryResult<'tcx>;

    // `Tuple` is implemented if the `Self` type is a tuple.
    fn consider_builtin_tuple_candidate(
        ecx: &mut EvalCtxt<'_, 'tcx>,
        goal: Goal<'tcx, Self>,
    ) -> QueryResult<'tcx>;

    // `Pointee` is always implemented.
    //
    // See the projection implementation for the `Metadata` types for all of
    // the built-in types. For structs, the metadata type is given by the struct
    // tail.
    fn consider_builtin_pointee_candidate(
        ecx: &mut EvalCtxt<'_, 'tcx>,
        goal: Goal<'tcx, Self>,
    ) -> QueryResult<'tcx>;

    // A generator (that comes from an `async` desugaring) is known to implement
    // `Future<Output = O>`, where `O` is given by the generator's return type
    // that was computed during type-checking.
    fn consider_builtin_future_candidate(
        ecx: &mut EvalCtxt<'_, 'tcx>,
        goal: Goal<'tcx, Self>,
    ) -> QueryResult<'tcx>;

    // A generator (that doesn't come from an `async` desugaring) is known to
    // implement `Generator<R, Yield = Y, Return = O>`, given the resume, yield,
    // and return types of the generator computed during type-checking.
    fn consider_builtin_generator_candidate(
        ecx: &mut EvalCtxt<'_, 'tcx>,
        goal: Goal<'tcx, Self>,
    ) -> QueryResult<'tcx>;

    // The most common forms of unsizing are array to slice, and concrete (Sized)
    // type into a `dyn Trait`. ADTs and Tuples can also have their final field
    // unsized if it's generic.
    fn consider_builtin_unsize_candidate(
        ecx: &mut EvalCtxt<'_, 'tcx>,
        goal: Goal<'tcx, Self>,
    ) -> QueryResult<'tcx>;

    // `dyn Trait1` can be unsized to `dyn Trait2` if they are the same trait, or
    // if `Trait2` is a (transitive) supertrait of `Trait2`.
    fn consider_builtin_dyn_upcast_candidates(
        ecx: &mut EvalCtxt<'_, 'tcx>,
        goal: Goal<'tcx, Self>,
    ) -> Vec<CanonicalResponse<'tcx>>;

    fn consider_builtin_discriminant_kind_candidate(
        ecx: &mut EvalCtxt<'_, 'tcx>,
        goal: Goal<'tcx, Self>,
    ) -> QueryResult<'tcx>;

    fn consider_builtin_destruct_candidate(
        ecx: &mut EvalCtxt<'_, 'tcx>,
        goal: Goal<'tcx, Self>,
    ) -> QueryResult<'tcx>;

    fn consider_builtin_transmute_candidate(
        ecx: &mut EvalCtxt<'_, 'tcx>,
        goal: Goal<'tcx, Self>,
    ) -> QueryResult<'tcx>;
}

impl<'tcx> EvalCtxt<'_, 'tcx> {
    pub(super) fn assemble_and_evaluate_candidates<G: GoalKind<'tcx>>(
        &mut self,
        goal: Goal<'tcx, G>,
    ) -> Vec<Candidate<'tcx>> {
        debug_assert_eq!(goal, self.resolve_vars_if_possible(goal));

        // HACK: `_: Trait` is ambiguous, because it may be satisfied via a builtin rule,
        // object bound, alias bound, etc. We are unable to determine this until we can at
        // least structurally resolve the type one layer.
        if goal.predicate.self_ty().is_ty_var() {
            return vec![Candidate {
                source: CandidateSource::BuiltinImpl,
                result: self
                    .evaluate_added_goals_and_make_canonical_response(Certainty::AMBIGUOUS)
                    .unwrap(),
            }];
        }

        let mut candidates = Vec::new();

        self.assemble_candidates_after_normalizing_self_ty(goal, &mut candidates);

        self.assemble_impl_candidates(goal, &mut candidates);

        self.assemble_builtin_impl_candidates(goal, &mut candidates);

        self.assemble_param_env_candidates(goal, &mut candidates);

        self.assemble_alias_bound_candidates(goal, &mut candidates);

        self.assemble_object_bound_candidates(goal, &mut candidates);

        self.assemble_coherence_unknowable_candidates(goal, &mut candidates);

        candidates
    }

    /// If the self type of a goal is an alias, computing the relevant candidates is difficult.
    ///
    /// To deal with this, we first try to normalize the self type and add the candidates for the normalized
    /// self type to the list of candidates in case that succeeds. We also have to consider candidates with the
    /// projection as a self type as well
    #[instrument(level = "debug", skip_all)]
    fn assemble_candidates_after_normalizing_self_ty<G: GoalKind<'tcx>>(
        &mut self,
        goal: Goal<'tcx, G>,
        candidates: &mut Vec<Candidate<'tcx>>,
    ) {
        let tcx = self.tcx();
        let &ty::Alias(_, projection_ty) = goal.predicate.self_ty().kind() else {
            return
        };

        let normalized_self_candidates: Result<_, NoSolution> = self.probe(
            |ecx| {
                ecx.with_incremented_depth(
                    |ecx| {
                        let result = ecx.evaluate_added_goals_and_make_canonical_response(
                            Certainty::Maybe(MaybeCause::Overflow),
                        )?;
                        Ok(vec![Candidate { source: CandidateSource::BuiltinImpl, result }])
                    },
                    |ecx| {
                        let normalized_ty = ecx.next_ty_infer();
                        let normalizes_to_goal = goal.with(
                            tcx,
                            ty::Binder::dummy(ty::ProjectionPredicate {
                                projection_ty,
                                term: normalized_ty.into(),
                            }),
                        );
                        ecx.add_goal(normalizes_to_goal);
                        let _ = ecx.try_evaluate_added_goals().inspect_err(|_| {
                            debug!("self type normalization failed");
                        })?;
                        let normalized_ty = ecx.resolve_vars_if_possible(normalized_ty);
                        debug!(?normalized_ty, "self type normalized");
                        // NOTE: Alternatively we could call `evaluate_goal` here and only
                        // have a `Normalized` candidate. This doesn't work as long as we
                        // use `CandidateSource` in winnowing.
                        let goal = goal.with(tcx, goal.predicate.with_self_ty(tcx, normalized_ty));
                        Ok(ecx.assemble_and_evaluate_candidates(goal))
                    },
                )
            },
            |_| CandidateKind::NormalizedSelfTyAssembly,
        );

        if let Ok(normalized_self_candidates) = normalized_self_candidates {
            candidates.extend(normalized_self_candidates);
        }
    }

    #[instrument(level = "debug", skip_all)]
    fn assemble_impl_candidates<G: GoalKind<'tcx>>(
        &mut self,
        goal: Goal<'tcx, G>,
        candidates: &mut Vec<Candidate<'tcx>>,
    ) {
        let tcx = self.tcx();
        tcx.for_each_relevant_impl_treating_projections(
            goal.predicate.trait_def_id(tcx),
            goal.predicate.self_ty(),
            TreatProjections::NextSolverLookup,
            |impl_def_id| match G::consider_impl_candidate(self, goal, impl_def_id) {
                Ok(result) => candidates
                    .push(Candidate { source: CandidateSource::Impl(impl_def_id), result }),
                Err(NoSolution) => (),
            },
        );
    }

    #[instrument(level = "debug", skip_all)]
    fn assemble_builtin_impl_candidates<G: GoalKind<'tcx>>(
        &mut self,
        goal: Goal<'tcx, G>,
        candidates: &mut Vec<Candidate<'tcx>>,
    ) {
        let lang_items = self.tcx().lang_items();
        let trait_def_id = goal.predicate.trait_def_id(self.tcx());

        // N.B. When assembling built-in candidates for lang items that are also
        // `auto` traits, then the auto trait candidate that is assembled in
        // `consider_auto_trait_candidate` MUST be disqualified to remain sound.
        //
        // Instead of adding the logic here, it's a better idea to add it in
        // `EvalCtxt::disqualify_auto_trait_candidate_due_to_possible_impl` in
        // `solve::trait_goals` instead.
        let result = if self.tcx().trait_is_auto(trait_def_id) {
            G::consider_auto_trait_candidate(self, goal)
        } else if self.tcx().trait_is_alias(trait_def_id) {
            G::consider_trait_alias_candidate(self, goal)
        } else if lang_items.sized_trait() == Some(trait_def_id) {
            G::consider_builtin_sized_candidate(self, goal)
        } else if lang_items.copy_trait() == Some(trait_def_id)
            || lang_items.clone_trait() == Some(trait_def_id)
        {
            G::consider_builtin_copy_clone_candidate(self, goal)
        } else if lang_items.pointer_like() == Some(trait_def_id) {
            G::consider_builtin_pointer_like_candidate(self, goal)
        } else if lang_items.fn_ptr_trait() == Some(trait_def_id) {
            G::consider_builtin_fn_ptr_trait_candidate(self, goal)
        } else if let Some(kind) = self.tcx().fn_trait_kind_from_def_id(trait_def_id) {
            G::consider_builtin_fn_trait_candidates(self, goal, kind)
        } else if lang_items.tuple_trait() == Some(trait_def_id) {
            G::consider_builtin_tuple_candidate(self, goal)
        } else if lang_items.pointee_trait() == Some(trait_def_id) {
            G::consider_builtin_pointee_candidate(self, goal)
        } else if lang_items.future_trait() == Some(trait_def_id) {
            G::consider_builtin_future_candidate(self, goal)
        } else if lang_items.gen_trait() == Some(trait_def_id) {
            G::consider_builtin_generator_candidate(self, goal)
        } else if lang_items.unsize_trait() == Some(trait_def_id) {
            G::consider_builtin_unsize_candidate(self, goal)
        } else if lang_items.discriminant_kind_trait() == Some(trait_def_id) {
            G::consider_builtin_discriminant_kind_candidate(self, goal)
        } else if lang_items.destruct_trait() == Some(trait_def_id) {
            G::consider_builtin_destruct_candidate(self, goal)
        } else if lang_items.transmute_trait() == Some(trait_def_id) {
            G::consider_builtin_transmute_candidate(self, goal)
        } else {
            Err(NoSolution)
        };

        match result {
            Ok(result) => {
                candidates.push(Candidate { source: CandidateSource::BuiltinImpl, result })
            }
            Err(NoSolution) => (),
        }

        // There may be multiple unsize candidates for a trait with several supertraits:
        // `trait Foo: Bar<A> + Bar<B>` and `dyn Foo: Unsize<dyn Bar<_>>`
        if lang_items.unsize_trait() == Some(trait_def_id) {
            for result in G::consider_builtin_dyn_upcast_candidates(self, goal) {
                candidates.push(Candidate { source: CandidateSource::BuiltinImpl, result });
            }
        }
    }

    #[instrument(level = "debug", skip_all)]
    fn assemble_param_env_candidates<G: GoalKind<'tcx>>(
        &mut self,
        goal: Goal<'tcx, G>,
        candidates: &mut Vec<Candidate<'tcx>>,
    ) {
        for (i, assumption) in goal.param_env.caller_bounds().iter().enumerate() {
            if let Some(clause) = assumption.as_clause() {
                match G::consider_implied_clause(self, goal, clause, []) {
                    Ok(result) => {
                        candidates.push(Candidate { source: CandidateSource::ParamEnv(i), result })
                    }
                    Err(NoSolution) => (),
                }
            }
        }
    }

    #[instrument(level = "debug", skip_all)]
    fn assemble_alias_bound_candidates<G: GoalKind<'tcx>>(
        &mut self,
        goal: Goal<'tcx, G>,
        candidates: &mut Vec<Candidate<'tcx>>,
    ) {
        let alias_ty = match goal.predicate.self_ty().kind() {
            ty::Bool
            | ty::Char
            | ty::Int(_)
            | ty::Uint(_)
            | ty::Float(_)
            | ty::Adt(_, _)
            | ty::Foreign(_)
            | ty::Str
            | ty::Array(_, _)
            | ty::Slice(_)
            | ty::RawPtr(_)
            | ty::Ref(_, _, _)
            | ty::FnDef(_, _)
            | ty::FnPtr(_)
            | ty::Dynamic(..)
            | ty::Closure(..)
            | ty::Generator(..)
            | ty::GeneratorWitness(_)
            | ty::GeneratorWitnessMIR(..)
            | ty::Never
            | ty::Tuple(_)
            | ty::Param(_)
            | ty::Placeholder(..)
            | ty::Infer(ty::IntVar(_) | ty::FloatVar(_))
            | ty::Alias(ty::Inherent, _)
            | ty::Alias(ty::Weak, _)
            | ty::Error(_) => return,
            ty::Infer(ty::TyVar(_) | ty::FreshTy(_) | ty::FreshIntTy(_) | ty::FreshFloatTy(_))
            | ty::Bound(..) => bug!("unexpected self type for `{goal:?}`"),
            // Excluding IATs and type aliases here as they don't have meaningful item bounds.
            ty::Alias(ty::Projection | ty::Opaque, alias_ty) => alias_ty,
        };

        for assumption in self.tcx().item_bounds(alias_ty.def_id).subst(self.tcx(), alias_ty.substs)
        {
            if let Some(clause) = assumption.as_clause() {
                match G::consider_alias_bound_candidate(self, goal, clause) {
                    Ok(result) => {
                        candidates.push(Candidate { source: CandidateSource::AliasBound, result })
                    }
                    Err(NoSolution) => (),
                }
            }
        }
    }

    /// Check that we are allowed to use an alias bound originating from the self
    /// type of this goal. This means something different depending on the self type's
    /// alias kind.
    ///
    /// * Projection: Given a goal with a self type such as `<Ty as Trait>::Assoc`,
    /// we require that the bound `Ty: Trait` can be proven using either a nested alias
    /// bound candidate, or a param-env candidate.
    ///
    /// * Opaque: The param-env must be in `Reveal::UserFacing` mode. Otherwise,
    /// the goal should be proven by using the hidden type instead.
    #[instrument(level = "debug", skip(self), ret)]
    pub(super) fn validate_alias_bound_self_from_param_env<G: GoalKind<'tcx>>(
        &mut self,
        goal: Goal<'tcx, G>,
    ) -> QueryResult<'tcx> {
        match *goal.predicate.self_ty().kind() {
            ty::Alias(ty::Projection, projection_ty) => {
                let mut param_env_candidates = vec![];
                let self_trait_ref = projection_ty.trait_ref(self.tcx());

                if self_trait_ref.self_ty().is_ty_var() {
                    return self
                        .evaluate_added_goals_and_make_canonical_response(Certainty::AMBIGUOUS);
                }

                let trait_goal: Goal<'_, ty::TraitPredicate<'tcx>> = goal.with(
                    self.tcx(),
                    ty::TraitPredicate {
                        trait_ref: self_trait_ref,
                        constness: ty::BoundConstness::NotConst,
                        polarity: ty::ImplPolarity::Positive,
                    },
                );

                self.assemble_param_env_candidates(trait_goal, &mut param_env_candidates);
                // FIXME: We probably need some sort of recursion depth check here.
                // Can't come up with an example yet, though, and the worst case
                // we can have is a compiler stack overflow...
                self.assemble_alias_bound_candidates(trait_goal, &mut param_env_candidates);

                // FIXME: We must also consider alias-bound candidates for a peculiar
                // class of built-in candidates that I'll call "defaulted" built-ins.
                //
                // For example, we always know that `T: Pointee` is implemented, but
                // we do not always know what `<T as Pointee>::Metadata` actually is,
                // similar to if we had a user-defined impl with a `default type ...`.
                // For these traits, since we're not able to always normalize their
                // associated types to a concrete type, we must consider their alias bounds
                // instead, so we can prove bounds such as `<T as Pointee>::Metadata: Copy`.
                self.assemble_alias_bound_candidates_for_builtin_impl_default_items(
                    trait_goal,
                    &mut param_env_candidates,
                );

                self.merge_candidates(param_env_candidates)
            }
            ty::Alias(ty::Opaque, _opaque_ty) => match goal.param_env.reveal() {
                Reveal::UserFacing => {
                    self.evaluate_added_goals_and_make_canonical_response(Certainty::Yes)
                }
                Reveal::All => return Err(NoSolution),
            },
            _ => bug!("only expected to be called on alias tys"),
        }
    }

    /// Assemble a subset of builtin impl candidates for a class of candidates called
    /// "defaulted" built-in traits.
    ///
    /// For example, we always know that `T: Pointee` is implemented, but we do not
    /// always know what `<T as Pointee>::Metadata` actually is! See the comment in
    /// [`EvalCtxt::validate_alias_bound_self_from_param_env`] for more detail.
    #[instrument(level = "debug", skip_all)]
    fn assemble_alias_bound_candidates_for_builtin_impl_default_items<G: GoalKind<'tcx>>(
        &mut self,
        goal: Goal<'tcx, G>,
        candidates: &mut Vec<Candidate<'tcx>>,
    ) {
        let lang_items = self.tcx().lang_items();
        let trait_def_id = goal.predicate.trait_def_id(self.tcx());

        // You probably shouldn't add anything to this list unless you
        // know what you're doing.
        let result = if lang_items.pointee_trait() == Some(trait_def_id) {
            G::consider_builtin_pointee_candidate(self, goal)
        } else if lang_items.discriminant_kind_trait() == Some(trait_def_id) {
            G::consider_builtin_discriminant_kind_candidate(self, goal)
        } else {
            Err(NoSolution)
        };

        match result {
            Ok(result) => {
                candidates.push(Candidate { source: CandidateSource::BuiltinImpl, result })
            }
            Err(NoSolution) => (),
        }
    }

    #[instrument(level = "debug", skip_all)]
    fn assemble_object_bound_candidates<G: GoalKind<'tcx>>(
        &mut self,
        goal: Goal<'tcx, G>,
        candidates: &mut Vec<Candidate<'tcx>>,
    ) {
        let self_ty = goal.predicate.self_ty();
        let bounds = match *self_ty.kind() {
            ty::Bool
            | ty::Char
            | ty::Int(_)
            | ty::Uint(_)
            | ty::Float(_)
            | ty::Adt(_, _)
            | ty::Foreign(_)
            | ty::Str
            | ty::Array(_, _)
            | ty::Slice(_)
            | ty::RawPtr(_)
            | ty::Ref(_, _, _)
            | ty::FnDef(_, _)
            | ty::FnPtr(_)
            | ty::Alias(..)
            | ty::Closure(..)
            | ty::Generator(..)
            | ty::GeneratorWitness(_)
            | ty::GeneratorWitnessMIR(..)
            | ty::Never
            | ty::Tuple(_)
            | ty::Param(_)
            | ty::Placeholder(..)
            | ty::Infer(ty::IntVar(_) | ty::FloatVar(_))
            | ty::Error(_) => return,
            ty::Infer(ty::TyVar(_) | ty::FreshTy(_) | ty::FreshIntTy(_) | ty::FreshFloatTy(_))
            | ty::Bound(..) => bug!("unexpected self type for `{goal:?}`"),
            ty::Dynamic(bounds, ..) => bounds,
        };

        let tcx = self.tcx();
        let own_bounds: FxIndexSet<_> =
            bounds.iter().map(|bound| bound.with_self_ty(tcx, self_ty)).collect();
        for assumption in elaborate(tcx, own_bounds.iter().copied())
            // we only care about bounds that match the `Self` type
            .filter_only_self()
        {
            // FIXME: Predicates are fully elaborated in the object type's existential bounds
            // list. We want to only consider these pre-elaborated projections, and not other
            // projection predicates that we reach by elaborating the principal trait ref,
            // since that'll cause ambiguity.
            //
            // We can remove this when we have implemented lifetime intersections in responses.
            if assumption.to_opt_poly_projection_pred().is_some()
                && !own_bounds.contains(&assumption)
            {
                continue;
            }

            if let Some(clause) = assumption.as_clause() {
                match G::consider_object_bound_candidate(self, goal, clause) {
                    Ok(result) => {
                        candidates.push(Candidate { source: CandidateSource::BuiltinImpl, result })
                    }
                    Err(NoSolution) => (),
                }
            }
        }
    }

    #[instrument(level = "debug", skip_all)]
    fn assemble_coherence_unknowable_candidates<G: GoalKind<'tcx>>(
        &mut self,
        goal: Goal<'tcx, G>,
        candidates: &mut Vec<Candidate<'tcx>>,
    ) {
        match self.solver_mode() {
            SolverMode::Normal => return,
            SolverMode::Coherence => {
                let trait_ref = goal.predicate.trait_ref(self.tcx());
                match coherence::trait_ref_is_knowable(self.tcx(), trait_ref) {
                    Ok(()) => {}
                    Err(_) => match self
                        .evaluate_added_goals_and_make_canonical_response(Certainty::AMBIGUOUS)
                    {
                        Ok(result) => candidates
                            .push(Candidate { source: CandidateSource::BuiltinImpl, result }),
                        // FIXME: This will be reachable at some point if we're in
                        // `assemble_candidates_after_normalizing_self_ty` and we get a
                        // universe error. We'll deal with it at this point.
                        Err(NoSolution) => bug!("coherence candidate resulted in NoSolution"),
                    },
                }
            }
        }
    }

    /// If there are multiple ways to prove a trait or projection goal, we have
    /// to somehow try to merge the candidates into one. If that fails, we return
    /// ambiguity.
    #[instrument(level = "debug", skip(self), ret)]
    pub(super) fn merge_candidates(
        &mut self,
        mut candidates: Vec<Candidate<'tcx>>,
    ) -> QueryResult<'tcx> {
        // First try merging all candidates. This is complete and fully sound.
        let responses = candidates.iter().map(|c| c.result).collect::<Vec<_>>();
        if let Some(result) = self.try_merge_responses(&responses) {
            return Ok(result);
        }

        // We then check whether we should prioritize `ParamEnv` candidates.
        //
        // Doing so is incomplete and would therefore be unsound during coherence.
        match self.solver_mode() {
            SolverMode::Coherence => (),
            // Prioritize `ParamEnv` candidates only if they do not guide inference.
            //
            // This is still incomplete as we may add incorrect region bounds.
            SolverMode::Normal => {
                let param_env_responses = candidates
                    .iter()
                    .filter(|c| {
                        matches!(
                            c.source,
                            CandidateSource::ParamEnv(_) | CandidateSource::AliasBound
                        )
                    })
                    .map(|c| c.result)
                    .collect::<Vec<_>>();
                if let Some(result) = self.try_merge_responses(&param_env_responses) {
                    // We strongly prefer alias and param-env bounds here, even if they affect inference.
                    // See https://github.com/rust-lang/trait-system-refactor-initiative/issues/11.
                    return Ok(result);
                }
            }
        }
        self.flounder(&responses)
    }
}
