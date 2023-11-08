#![allow(dead_code, unused_imports, unused_variables)]

use super::ancillary::AncillaryData;
use crate::ffi::c_int;
use crate::io::{self, IoSlice, IoSliceMut};

#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
pub trait SendMessage {
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    fn send_message(
        &self,
        bufs: &[IoSlice<'_>],
        ancillary_data: &mut AncillaryData<'_, '_>,
        options: SendOptions,
    ) -> io::Result<usize>;
}

#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
pub trait SendMessageTo {
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    type SocketAddr;

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    fn send_message_to(
        &self,
        addr: &Self::SocketAddr,
        bufs: &[IoSlice<'_>],
        ancillary_data: &mut AncillaryData<'_, '_>,
        options: SendOptions,
    ) -> io::Result<usize>;
}

#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
#[derive(Copy, Clone)]
pub struct SendOptions {
    bits: c_int,
}

impl SendOptions {
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn new() -> SendOptions {
        SendOptions { bits: 0 }
    }

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn as_send_flags(&self) -> c_int {
        self.bits
    }

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn as_sendmsg_flags(&self) -> c_int {
        self.bits
    }

    // https://doc.rust-lang.org/std/os/unix/fs/trait.OpenOptionsExt.html
    // custom_flags(&mut self, flags: i32) -> &mut Self

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn set_eor(&mut self, eor: bool) -> &mut Self {
        let _ = eor;
        todo!()
    }

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn set_eob(&mut self, eob: bool) -> &mut Self {
        let _ = eob;
        todo!()
    }

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn set_no_signal(&mut self, no_signal: bool) -> &mut Self {
        let _ = no_signal;
        todo!()
    }
}

#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
impl Default for SendOptions {
    fn default() -> SendOptions {
        SendOptions::new()
    }
}

#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
pub struct MessageSender<'a, 'b, S> {
    buf: SenderBuf<'a>,
    socket: &'a S,
    options: SendOptions,
    ancillary_data: Option<&'a mut AncillaryData<'b, 'b>>,
}

#[derive(Copy, Clone)]
enum SenderBuf<'a> {
    Buf([IoSlice<'a>; 1]),
    Bufs(&'a [IoSlice<'a>]),
}

impl<'a> SenderBuf<'a> {
    fn get(&self) -> &[IoSlice<'a>] {
        match self {
            SenderBuf::Buf(ref buf) => buf,
            SenderBuf::Bufs(bufs) => bufs,
        }
    }
}

impl<'a, 'b, S> MessageSender<'a, 'b, S> {
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn new(socket: &'a S, buf: &'a [u8]) -> MessageSender<'a, 'b, S> {
        MessageSender {
            buf: SenderBuf::Buf([IoSlice::new(buf)]),
            socket,
            options: SendOptions::new(),
            ancillary_data: None,
        }
    }

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn new_vectored(socket: &'a S, bufs: &'a [IoSlice<'a>]) -> MessageSender<'a, 'b, S> {
        MessageSender {
            buf: SenderBuf::Bufs(bufs),
            socket,
            options: SendOptions::new(),
            ancillary_data: None,
        }
    }

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn ancillary_data(&mut self, ancillary_data: &'a mut AncillaryData<'b, 'b>) -> &mut Self {
        self.ancillary_data = Some(ancillary_data);
        self
    }
}

impl<S: SendMessage> MessageSender<'_, '_, S> {
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn send(&mut self) -> io::Result<usize> {
        let mut ancillary_empty = AncillaryData::new(&mut []);
        let ancillary_data = match self.ancillary_data {
            Some(ref mut x) => x,
            None => &mut ancillary_empty,
        };
        self.socket.send_message(self.buf.get(), ancillary_data, self.options)
    }
}

impl<S: SendMessageTo> MessageSender<'_, '_, S> {
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn send_to(&mut self, addr: &S::SocketAddr) -> io::Result<usize> {
        let mut ancillary_empty = AncillaryData::new(&mut []);
        let ancillary_data = match self.ancillary_data {
            Some(ref mut x) => x,
            None => &mut ancillary_empty,
        };
        self.socket.send_message_to(addr, self.buf.get(), ancillary_data, self.options)
    }
}

#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
pub trait RecvMessage {
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    fn recv_message(
        &self,
        bufs: &mut [IoSliceMut<'_>],
        ancillary_data: &mut AncillaryData<'_, '_>,
        options: RecvOptions,
    ) -> io::Result<(usize, MessageFlags)>;
}

#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
pub trait RecvMessageFrom {
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    type SocketAddr;

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    fn recv_message_from(
        &self,
        bufs: &mut [IoSliceMut<'_>],
        ancillary_data: &mut AncillaryData<'_, '_>,
        options: RecvOptions,
    ) -> io::Result<(usize, MessageFlags, Self::SocketAddr)>;
}

#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
#[derive(Copy, Clone)]
pub struct RecvOptions {
    bits: c_int,
}

impl RecvOptions {
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn new() -> RecvOptions {
        RecvOptions { bits: 0 }
    }

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn as_recv_flags(&self) -> c_int {
        self.bits
    }

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn as_recvmsg_flags(&self) -> c_int {
        self.bits | libc::MSG_CMSG_CLOEXEC
    }

    // https://doc.rust-lang.org/std/os/unix/fs/trait.OpenOptionsExt.html
    // custom_flags(&mut self, flags: i32) -> &mut Self

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn set_oob(&mut self, oob: bool) -> &mut Self {
        let _ = oob;
        todo!()
    }

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn set_peek(&mut self, peek: bool) -> &mut Self {
        let _ = peek;
        todo!()
    }

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn set_waitall(&mut self, waitall: bool) -> &mut Self {
        let _ = waitall;
        todo!()
    }
}

#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
impl Default for RecvOptions {
    fn default() -> RecvOptions {
        RecvOptions::new()
    }
}

#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
#[derive(Copy, Clone)]
pub struct MessageFlags {
    raw: c_int,
}

impl MessageFlags {
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn from_raw(raw: c_int) -> MessageFlags {
        MessageFlags { raw }
    }

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn end_of_record(&self) -> bool {
        todo!()
    }

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn oob_received(&self) -> bool {
        todo!()
    }

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn normal_data_truncated(&self) -> bool {
        todo!()
    }

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn control_data_truncated(&self) -> bool {
        todo!()
    }
}

#[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
pub struct MessageReceiver<'a, 'b, S> {
    buf: ReceiverBuf<'a>,
    socket: &'a S,
    options: RecvOptions,
    ancillary_data: Option<&'a mut AncillaryData<'b, 'static>>,
}

enum ReceiverBuf<'a> {
    Buf([IoSliceMut<'a>; 1]),
    Bufs(&'a mut [IoSliceMut<'a>]),
}

impl<'a> ReceiverBuf<'a> {
    fn get(&mut self) -> &mut [IoSliceMut<'a>] {
        match self {
            ReceiverBuf::Buf(ref mut buf) => buf,
            ReceiverBuf::Bufs(bufs) => bufs,
        }
    }
}

impl<'a, 'b, S> MessageReceiver<'a, 'b, S> {
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn new(socket: &'a S, buf: &'a mut [u8]) -> MessageReceiver<'a, 'b, S> {
        Self {
            buf: ReceiverBuf::Buf([IoSliceMut::new(buf)]),
            socket,
            options: RecvOptions::new(),
            ancillary_data: None,
        }
    }

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn new_vectored(
        socket: &'a S,
        bufs: &'a mut [IoSliceMut<'a>],
    ) -> MessageReceiver<'a, 'b, S> {
        Self {
            buf: ReceiverBuf::Bufs(bufs),
            socket,
            options: RecvOptions::new(),
            ancillary_data: None,
        }
    }

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn options(&mut self, options: RecvOptions) -> &mut Self {
        self.options = options;
        self
    }

    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn ancillary_data(
        &mut self,
        ancillary_data: &'a mut AncillaryData<'b, 'static>,
    ) -> &mut Self {
        self.ancillary_data = Some(ancillary_data);
        self
    }
}

impl<S: RecvMessage> MessageReceiver<'_, '_, S> {
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn recv(&mut self) -> io::Result<(usize, MessageFlags)> {
        let mut ancillary_empty = AncillaryData::new(&mut []);
        let ancillary_data = match self.ancillary_data {
            Some(ref mut x) => x,
            None => &mut ancillary_empty,
        };
        self.socket.recv_message(self.buf.get(), ancillary_data, self.options)
    }
}

impl<S: RecvMessageFrom> MessageReceiver<'_, '_, S> {
    #[unstable(feature = "unix_socket_ancillary_data", issue = "76915")]
    pub fn recv_from(&mut self) -> io::Result<(usize, MessageFlags, S::SocketAddr)> {
        let mut ancillary_empty = AncillaryData::new(&mut []);
        let ancillary_data = match self.ancillary_data {
            Some(ref mut x) => x,
            None => &mut ancillary_empty,
        };
        self.socket.recv_message_from(self.buf.get(), ancillary_data, self.options)
    }
}
