#![allow(dead_code, unused_imports, unused_variables)]

use crate::collections::TryReserveError;
use crate::ffi::c_int;
use crate::mem::{size_of, MaybeUninit};
use crate::os::unix::io::{BorrowedFd, FromRawFd, OwnedFd, RawFd};

// Wrapper around `libc::CMSG_LEN` to safely decouple from OS-specific ints.
//
// https://github.com/rust-lang/libc/issues/3240
#[inline]
const fn CMSG_LEN(len: usize) -> usize {
    let c_len = len & 0x7FFFFFFF;
    let padding = (unsafe { libc::CMSG_LEN(c_len as _) } as usize) - c_len;
    len + padding
}

// Wrapper around `libc::CMSG_SPACE` to safely decouple from OS-specific ints.
//
// https://github.com/rust-lang/libc/issues/3240
#[inline]
const fn CMSG_SPACE(len: usize) -> usize {
    let c_len = len & 0x7FFFFFFF;
    let padding = (unsafe { libc::CMSG_SPACE(c_len as _) } as usize) - c_len;
    len + padding
}

const FD_SIZE: usize = size_of::<RawFd>();

/// A socket control message with borrowed data.
///
/// This type is semantically equivalent to POSIX `struct cmsghdr`, but is
/// not guaranteed to have the same internal representation.
#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ControlMessage<'a> {
    cmsg_len: usize,
    cmsg_level: c_int,
    cmsg_type: c_int,
    data: &'a [u8],
}

impl<'a> ControlMessage<'a> {
    /// Creates a `ControlMessage` with the given level, type, and data.
    ///
    /// The semantics of a control message "level" and "type" are OS-specific,
    /// but generally the level is a sort of general category of socket and the
    /// type identifies a specific control message data layout.
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn new(cmsg_level: c_int, cmsg_type: c_int, data: &'a [u8]) -> ControlMessage<'a> {
        let cmsg_len = CMSG_LEN(data.len());
        ControlMessage { cmsg_len, cmsg_level, cmsg_type, data }
    }
}

impl ControlMessage<'_> {
    /// Returns the control message's level, an OS-specific value.
    ///
    /// POSIX describes this field as the "originating protocol".
    #[inline]
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn cmsg_level(&self) -> c_int {
        self.cmsg_level
    }

    /// Returns the control message's type, an OS-specific value.
    ///
    /// POSIX describes this field as the "protocol-specific type".
    #[inline]
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn cmsg_type(&self) -> c_int {
        self.cmsg_type
    }

    /// Returns the control message's type-specific data.
    ///
    /// The returned slice is equivalent to the result of C macro `CMSG_DATA()`.
    /// Control message data is not guaranteed to be aligned, so code that needs
    /// to inspect it should first copy the data to a properly-aligned location.
    #[inline]
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn data(&self) -> &[u8] {
        self.data
    }

    /// Returns `true` if the control message data is truncated.
    ///
    /// The kernel may truncate a control message if its data is too large to
    /// fit into the capacity of the userspace buffer.
    ///
    /// The semantics of truncated control messages are OS- and type-specific.
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn truncated(&self) -> bool {
        self.cmsg_len > CMSG_LEN(self.data.len())
    }

    #[inline]
    pub(super) fn cmsg_space(&self) -> usize {
        CMSG_SPACE(self.data.len())
    }

    pub(super) fn copy_to_slice<'a>(&self, dst: &'a mut [MaybeUninit<u8>]) -> &'a [u8] {
        assert_eq!(dst.len(), self.cmsg_space());

        // SAFETY: C type `struct cmsghdr` is safe to zero-initialize.
        let mut hdr: libc::cmsghdr = unsafe { core::mem::zeroed() };

        // Write `cmsg.cmsg_len` instead of `CMSG_LEN(data.len())` so that
        // truncated control messages are preserved as-is.
        hdr.cmsg_len = self.cmsg_len as _;
        hdr.cmsg_level = self.cmsg_level;
        hdr.cmsg_type = self.cmsg_type;

        #[inline]
        unsafe fn sized_to_slice<T: Sized>(t: &T) -> &[u8] {
            let t_ptr = (t as *const T).cast::<u8>();
            crate::slice::from_raw_parts(t_ptr, size_of::<T>())
        }

        let (hdr_dst, after_hdr) = dst.split_at_mut(size_of::<libc::cmsghdr>());
        let (data_dst, padding_dst) = after_hdr.split_at_mut(self.data.len());

        // SAFETY: C type `struct cmsghdr` is safe to bitwise-copy from.
        MaybeUninit::write_slice(hdr_dst, unsafe { sized_to_slice(&hdr) });

        // See comment in `ControlMessagesIter` regarding `CMSG_DATA()`.
        MaybeUninit::write_slice(data_dst, self.data());

        if padding_dst.len() > 0 {
            for byte in padding_dst.iter_mut() {
                byte.write(0);
            }
        }

        // SAFETY: Every byte in `dst` has been initialized.
        unsafe { MaybeUninit::slice_assume_init_ref(dst) }
    }
}

/// A borrowed reference to a `&[u8]` slice containing control messages.
///
/// Note that this type does not guarantee the control messages are valid, or
/// even well-formed. Code that uses control messages to implement (for example)
/// access control or file descriptor passing should maintain a chain of custody
/// to verify that the `&ControlMessages` came from a trusted source, such as
/// a syscall.
#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
pub struct ControlMessages {
    bytes: [u8],
}

impl ControlMessages {
    /// Creates a `ControlMessages` wrapper from a `&[u8]` slice containing
    /// encoded control messages.
    ///
    /// This method does not attempt to verify that the provided bytes represent
    /// valid control messages.
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn from_bytes(bytes: &[u8]) -> &ControlMessages {
        // SAFETY: casting `&[u8]` to `&ControlMessages` is safe because its
        // internal representation is `[u8]`.
        unsafe { &*(bytes as *const [u8] as *const ControlMessages) }
    }

    /// Returns a `&[u8]` slice containing encoded control messages.
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns `true` if `self.as_bytes()` is an empty slice.
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Returns an iterator over the control messages.
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn iter(&self) -> ControlMessagesIter<'_> {
        ControlMessagesIter { bytes: &self.bytes }
    }
}

#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
impl<'a> IntoIterator for &'a ControlMessages {
    type Item = ControlMessage<'a>;
    type IntoIter = ControlMessagesIter<'a>;

    fn into_iter(self) -> ControlMessagesIter<'a> {
        self.iter()
    }
}

/// An iterator over the content of a [`ControlMessages`].
///
/// Each control message starts with a header describing its own length. This
/// iterator is safe even if the header lengths are incorrect, but the returned
/// control messages may contain incorrect data.
///
/// Iteration ends when the remaining data is smaller than the size of a single
/// control message header.
#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
pub struct ControlMessagesIter<'a> {
    bytes: &'a [u8],
}

impl<'a> ControlMessagesIter<'a> {
    /// Returns a `&[u8]` slice containing any remaining data.
    ///
    /// Even if `next()` returns `None`, this method may return a non-empty
    /// slice if the original `ControlMessages` was truncated in the middle
    /// of a control message header.
    #[inline]
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn into_bytes(self) -> &'a [u8] {
        self.bytes
    }
}

#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
impl<'a> Iterator for ControlMessagesIter<'a> {
    type Item = ControlMessage<'a>;

    fn next(&mut self) -> Option<ControlMessage<'a>> {
        const CMSGHDR_SIZE: usize = size_of::<libc::cmsghdr>();

        if CMSGHDR_SIZE > self.bytes.len() {
            return None;
        }

        // SAFETY: C type `struct cmsghdr` is safe to bitwise-copy from.
        let hdr = unsafe {
            let mut hdr = MaybeUninit::<libc::cmsghdr>::uninit();
            hdr.as_mut_ptr().cast::<u8>().copy_from(self.bytes.as_ptr(), CMSGHDR_SIZE);
            hdr.assume_init()
        };

        // `cmsg_bytes` contains the full content of the control message,
        // which may have been truncated if there was insufficient capacity.
        let cmsg_bytes;
        let hdr_cmsg_len = hdr.cmsg_len as usize;
        if hdr_cmsg_len >= self.bytes.len() {
            cmsg_bytes = self.bytes;
        } else {
            cmsg_bytes = &self.bytes[..hdr_cmsg_len];
        }

        // `cmsg_data` is the portion of the control message that contains
        // type-specific content (file descriptors, etc).
        //
        // POSIX specifies that a pointer to this data should be obtained with
        // macro `CMSG_DATA()`, but its definition is problematic for Rust:
        //
        //   1. The macro may in principle read fields of `cmsghdr`. To avoid
        //      unaligned reads this code would call it as `CMSG_DATA(&hdr)`.
        //      But the resulting pointer would be relative to the stack value
        //      `hdr`, not the actual message data contained in `cmsg_bytes`.
        //
        //   2. `CMSG_DATA()` is implemented with `pointer::offset()`, which
        //      causes undefined behavior if its result is outside the original
        //      allocated object. The POSIX spec allows control messages to
        //      have padding between the header and data, in which case
        //      `CMSG_DATA(&hdr)` is UB.
        //
        //   3. The control message may have been truncated. We know there's
        //      at least `CMSGHDR_SIZE` bytes available, but anything past that
        //      isn't guaranteed. Again, possible UB in the presence of padding.
        //
        // Therefore, this code obtains `cmsg_data` by assuming it directly
        // follows the header (with no padding, and no header field dependency).
        // This is true on all target OSes currently supported by Rust.
        //
        // If in the future support is added for an OS with cmsg data padding,
        // then this implementation will cause unit test failures rather than
        // risking silent UB.
        let cmsg_data = &cmsg_bytes[CMSGHDR_SIZE..];

        // `cmsg_space` is the length of the control message plus any padding
        // necessary to align the next message.
        let cmsg_space = CMSG_SPACE(cmsg_data.len());
        if cmsg_space >= self.bytes.len() {
            self.bytes = &[];
        } else {
            self.bytes = &self.bytes[cmsg_space..];
        }

        Some(ControlMessage {
            cmsg_len: hdr_cmsg_len,
            cmsg_level: hdr.cmsg_level,
            cmsg_type: hdr.cmsg_type,
            data: cmsg_data,
        })
    }
}

#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
pub struct AncillaryData<'a, 'fd> {
    cmsgs_buf: &'a mut [MaybeUninit<u8>],
    cmsgs_len: usize,
    cmsgs_buf_fully_initialized: bool,
    scm_rights_received: bool,
    scm_rights_max_len: Option<usize>,
    borrowed_fds: core::marker::PhantomData<[BorrowedFd<'fd>]>,
}

#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
pub struct AncillaryDataNoCapacity {
    _p: (),
}

#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
impl Drop for AncillaryData<'_, '_> {
    fn drop(&mut self) {
        drop(self.received_fds())
    }
}

impl<'a, 'fd> AncillaryData<'a, 'fd> {
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn new(control_messages_buf: &'a mut [MaybeUninit<u8>]) -> AncillaryData<'a, 'fd> {
        let cmsgs_buf_fully_initialized = control_messages_buf.is_empty();
        AncillaryData {
            cmsgs_buf: control_messages_buf,
            cmsgs_len: 0,
            cmsgs_buf_fully_initialized,
            scm_rights_received: false,
            scm_rights_max_len: None,
            borrowed_fds: core::marker::PhantomData,
        }
    }

    fn cmsgs_buf(&self) -> &[u8] {
        let init_part = &self.cmsgs_buf[..self.cmsgs_len];
        unsafe { MaybeUninit::slice_assume_init_ref(init_part) }
    }

    fn cmsgs_buf_mut(&mut self) -> &mut [u8] {
        let init_part = &mut self.cmsgs_buf[..self.cmsgs_len];
        unsafe { MaybeUninit::slice_assume_init_mut(init_part) }
    }

    // returns initialized portion of `control_messages_buf`.
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn control_messages(&self) -> &ControlMessages {
        ControlMessages::from_bytes(self.cmsgs_buf())
    }

    // copy a control message into the ancillary data; error on out-of-capacity.
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn add_control_message<'b>(
        &mut self,
        control_message: impl Into<ControlMessage<'b>>,
    ) -> Result<(), AncillaryDataNoCapacity> {
        let cmsg = control_message.into();
        self.add_cmsg(&cmsg)
    }

    fn add_cmsg(&mut self, cmsg: &ControlMessage<'_>) -> Result<(), AncillaryDataNoCapacity> {
        let cmsg_len = cmsg.cmsg_space();
        if self.cmsgs_len + cmsg_len > self.cmsgs_buf.len() {
            return Err(AncillaryDataNoCapacity { _p: () });
        }

        let (_, spare_capacity) = self.cmsgs_buf.split_at_mut(self.cmsgs_len);
        let copied = cmsg.copy_to_slice(&mut spare_capacity[..cmsg_len]).len();
        assert_eq!(cmsg_len, copied);
        self.cmsgs_len += cmsg_len;
        Ok(())
    }

    // Add an `SCM_RIGHTS` control message with given borrowed FDs.
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn add_file_descriptors(
        &mut self,
        borrowed_fds: &[BorrowedFd<'fd>],
    ) -> Result<(), AncillaryDataNoCapacity> {
        let data_ptr = borrowed_fds.as_ptr().cast::<u8>();
        let data_len = borrowed_fds.len() * size_of::<RawFd>();
        let data = unsafe { crate::slice::from_raw_parts(data_ptr, data_len) };
        let cmsg = ControlMessage::new(libc::SOL_SOCKET, libc::SCM_RIGHTS, data);
        self.add_cmsg(&cmsg)
    }

    // Transfers ownership of received FDs to the iterator.
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn received_fds(&mut self) -> AncillaryDataReceivedFds<'_> {
        if !self.scm_rights_received {
            return AncillaryDataReceivedFds { buf: None };
        }

        assert!(self.scm_rights_max_len.is_some());
        let max_len = self.scm_rights_max_len.unwrap();
        let cmsgs_buf_might_contain_fds = &mut self.cmsgs_buf_mut()[..max_len];
        AncillaryDataReceivedFds { buf: Some(cmsgs_buf_might_contain_fds) }
    }

    // Obtain a mutable buffer usable as the `msg_control` pointer in a call
    // to `sendmsg()` or `recvmsg()`.
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn control_messages_buf(&mut self) -> Option<&mut [u8]> {
        if self.cmsgs_buf.len() == 0 {
            return None;
        }

        let (_, spare_capacity) = self.cmsgs_buf.split_at_mut(self.cmsgs_len);
        // TODO: replace with https://github.com/rust-lang/rust/pull/117426
        for byte in spare_capacity {
            byte.write(0);
        }
        self.cmsgs_buf_fully_initialized = true;
        self.cmsgs_len = 0;
        self.scm_rights_received = false;
        self.scm_rights_max_len = None;
        let buf = unsafe { MaybeUninit::slice_assume_init_mut(self.cmsgs_buf) };
        Some(buf)
    }

    // Update the control messages buffer length according to the result of
    // calling `sendmsg()` or `recvmsg()`.
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn set_control_messages_len(&mut self, len: usize) {
        assert!(self.cmsgs_buf.len() >= len);
        assert!(self.cmsgs_buf_fully_initialized);
        if self.cmsgs_buf.len() > 0 {
            self.cmsgs_len = len;
            self.cmsgs_buf_fully_initialized = false;
        }
        self.scm_rights_max_len = Some(len);
    }

    // Take ownership of any file descriptors in the control messages buffer.
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub unsafe fn take_ownership_of_scm_rights(&mut self) {
        assert!(self.scm_rights_max_len.is_some());
        self.scm_rights_received = true;
    }
}

#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
pub struct AncillaryDataReceivedFds<'a> {
    buf: Option<&'a mut [u8]>,
}

#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
impl Drop for AncillaryDataReceivedFds<'_> {
    fn drop(&mut self) {
        while let Some(_) = self.next() {}
    }
}

impl AncillaryDataReceivedFds<'_> {
    fn advance_to_next_scm_rights(mut buf: &mut [u8]) -> Option<(&mut [u8], usize, usize)> {
        loop {
            let cmsg = ControlMessagesIter { bytes: buf }.next()?;
            let cmsg_size = cmsg.cmsg_space();
            let data_len = cmsg.data().len();
            if Self::is_scm_rights(&cmsg) && data_len > 0 {
                return Some((buf, cmsg_size, data_len));
            }
            buf = &mut buf[cmsg_size..];
        }
    }

    fn take_next_fd(mut buf: &mut [u8]) -> Option<(&mut [u8], OwnedFd)> {
        loop {
            let cmsg_space;
            let cmsg_data_len;
            (buf, cmsg_space, cmsg_data_len) = Self::advance_to_next_scm_rights(buf)?;

            // If an owned FD can be found in the current `SCM_RIGHTS`, take
            // ownership and return it. Otherwise, advance and look for any
            // additional `SCM_RIGHTS` that might have been received (for
            // platforms that don't coalesce them).
            let scm_rights =
                &mut buf[size_of::<libc::cmsghdr>()..size_of::<libc::cmsghdr>() + cmsg_data_len];
            let scm_rights_fds = &mut scm_rights[..];

            let Some(fd_buf) = Self::next_owned_fd(scm_rights_fds) else {
                buf = &mut buf[cmsg_space..];
                continue;
            };

            let raw_fd = RawFd::from_ne_bytes(*fd_buf);
            let owned_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
            let mark_as_taken: RawFd = -1;
            fd_buf.clone_from_slice(&mark_as_taken.to_ne_bytes());
            return Some((buf, owned_fd));
        }
    }

    fn is_scm_rights(cmsg: &ControlMessage<'_>) -> bool {
        cmsg.cmsg_level == libc::SOL_SOCKET && cmsg.cmsg_type == libc::SCM_RIGHTS
    }

    fn next_owned_fd(mut data: &mut [u8]) -> Option<&mut [u8; FD_SIZE]> {
        loop {
            if FD_SIZE > data.len() {
                // Don't try to inspect a fragmentary FD in a truncated message.
                return None;
            }
            let chunk_slice;
            (chunk_slice, data) = data.split_at_mut(FD_SIZE);
            let chunk: &mut [u8; FD_SIZE] = chunk_slice.try_into().unwrap();
            if RawFd::from_ne_bytes(*chunk) != -1 {
                return Some(chunk);
            }
        }
    }
}

#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
impl<'a> Iterator for AncillaryDataReceivedFds<'a> {
    type Item = OwnedFd;

    fn next(&mut self) -> Option<OwnedFd> {
        let buf = self.buf.take()?;
        let (new_buf, next_fd) = Self::take_next_fd(buf)?;
        self.buf = Some(new_buf);
        Some(next_fd)
    }
}

#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
pub struct AncillaryDataBuf<'fd> {
    cmsgs_buf: Vec<u8>,
    borrowed_fds: core::marker::PhantomData<[BorrowedFd<'fd>]>,
}

impl<'fd> AncillaryDataBuf<'fd> {
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn new() -> AncillaryDataBuf<'fd> {
        AncillaryDataBuf { cmsgs_buf: Vec::new(), borrowed_fds: core::marker::PhantomData }
    }

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn with_capacity(capacity: usize) -> AncillaryDataBuf<'fd> {
        AncillaryDataBuf {
            cmsgs_buf: Vec::with_capacity(capacity),
            borrowed_fds: core::marker::PhantomData,
        }
    }

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn capacity(&self) -> usize {
        self.cmsgs_buf.capacity()
    }

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn control_messages(&self) -> &ControlMessages {
        ControlMessages::from_bytes(&self.cmsgs_buf)
    }

    // copy a control message into the ancillary data; panic on alloc failure.
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn add_control_message<'a>(&mut self, control_message: impl Into<ControlMessage<'a>>) {
        self.add_cmsg(&control_message.into());
    }

    fn add_cmsg(&mut self, cmsg: &ControlMessage<'_>) {
        let cmsg_len = cmsg.cmsg_space();
        let cmsgs_len = self.cmsgs_buf.len();

        self.cmsgs_buf.reserve(cmsg_len);
        let spare_capacity = self.cmsgs_buf.spare_capacity_mut();
        let copied = cmsg.copy_to_slice(&mut spare_capacity[..cmsg_len]).len();
        assert_eq!(cmsg_len, copied);
        unsafe {
            self.cmsgs_buf.set_len(cmsgs_len + cmsg_len);
        }
    }

    // Add an `SCM_RIGHTS` control message with given borrowed FDs; panic on
    // alloc failure.
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn add_file_descriptors(&mut self, borrowed_fds: &[BorrowedFd<'fd>]) {
        let data_ptr = borrowed_fds.as_ptr().cast::<u8>();
        let data_len = borrowed_fds.len() * size_of::<RawFd>();
        let data = unsafe { crate::slice::from_raw_parts(data_ptr, data_len) };
        let cmsg = ControlMessage::new(libc::SOL_SOCKET, libc::SCM_RIGHTS, data);
        self.add_cmsg(&cmsg);
    }

    // Used to obtain `AncillaryData` for passing to send/recv calls.
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn to_ancillary_data(&mut self) -> AncillaryData<'_, 'fd> {
        // Transfer ownership of control messages into the `AncillaryData`.
        let cmsgs_len = self.cmsgs_buf.len();
        self.cmsgs_buf.clear();
        AncillaryData {
            cmsgs_buf: self.cmsgs_buf.spare_capacity_mut(),
            cmsgs_len: cmsgs_len,
            cmsgs_buf_fully_initialized: false,
            scm_rights_received: false,
            scm_rights_max_len: None,
            borrowed_fds: core::marker::PhantomData,
        }
    }

    // Clears the control message buffer, without affecting capacity.
    //
    // This will not leak FDs because the `AncillaryData` type holds a mutable
    // reference to the `AncillaryDataBuf`, so if `clear()` is called then there
    // are no outstanding `AncillaryData`s and thus no received FDs.
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn clear(&mut self) {
        self.cmsgs_buf.clear();
    }

    // as in Vec
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn reserve(&mut self, capacity: usize) {
        self.cmsgs_buf.reserve(capacity);
    }

    // as in Vec
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn reserve_exact(&mut self, capacity: usize) {
        self.cmsgs_buf.reserve_exact(capacity);
    }

    // as in Vec
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn try_reserve(&mut self, capacity: usize) -> Result<(), TryReserveError> {
        self.cmsgs_buf.try_reserve(capacity)
    }

    // as in Vec
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn try_reserve_exact(&mut self, capacity: usize) -> Result<(), TryReserveError> {
        self.cmsgs_buf.try_reserve_exact(capacity)
    }
}

#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
impl<'a> Extend<ControlMessage<'a>> for AncillaryDataBuf<'_> {
    fn extend<I>(&mut self, iter: I)
    where
        I: core::iter::IntoIterator<Item = ControlMessage<'a>>,
    {
        for cmsg in iter {
            self.add_cmsg(&cmsg);
        }
    }
}

#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
impl<'a> Extend<&'a ControlMessage<'a>> for AncillaryDataBuf<'_> {
    fn extend<I>(&mut self, iter: I)
    where
        I: core::iter::IntoIterator<Item = &'a ControlMessage<'a>>,
    {
        for cmsg in iter {
            self.add_cmsg(cmsg);
        }
    }
}
