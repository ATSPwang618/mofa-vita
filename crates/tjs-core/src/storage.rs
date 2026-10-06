//! Host stream boundary for language container I/O. Storage names and mode
//! strings remain UTF-16 so archive/VFS hosts can implement the same contract.
use crate::{
    Heap, NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, Trace, Value,
};

pub enum Io {
    ReadText,
    ReadBinary,
    WriteText(Vec<u16>),
    WriteBinary(Vec<u8>),
}
pub struct Request {
    pub name: Vec<u16>,
    pub mode: Vec<u16>,
    pub io: Io,
}
pub type Delegate =
    fn(&mut NativeCx<'_>, Request, Box<dyn NativeContinuation>) -> NativeResult<NativeStep>;

pub trait Storage: Trace {
    /// Hosts with script-owned storage resolve it on the caller's VM through
    /// this continuation hook. Plain filesystem hosts keep synchronous defaults.
    fn delegate(&self) -> Option<Delegate> {
        None
    }
    /// Bound decoded script text as well as the bytes returned by the adapter.
    fn max_read_bytes(&self) -> usize {
        usize::MAX / 2
    }
    fn read_text(&mut self, name: &[u16], mode: &[u16]) -> NativeResult<Vec<u16>>;
    fn write_text(&mut self, name: &[u16], mode: &[u16], text: &[u16]) -> NativeResult<()>;
    fn read_binary(&mut self, name: &[u16], mode: &[u16]) -> NativeResult<Vec<u8>>;
    fn write_binary(&mut self, name: &[u16], mode: &[u16], bytes: &[u8]) -> NativeResult<()>;
}
impl NativeCx<'_> {
    pub fn storage_io(
        &mut self,
        request: Request,
        next: Box<dyn NativeContinuation>,
    ) -> NativeResult<NativeStep> {
        if let Some(delegate) = self.heap_mut().storage()?.delegate() {
            delegate(self, request, next)
        } else {
            self.storage_io_direct(request, next)
        }
    }
    /// Run the host's plain IO methods without re-entering its managed resolver.
    pub fn storage_io_direct(
        &mut self,
        request: Request,
        next: Box<dyn NativeContinuation>,
    ) -> NativeResult<NativeStep> {
        let Request { name, mode, io } = request;
        let value = match io {
            Io::ReadText => {
                let text = self.heap_mut().storage()?.read_text(&name, &mode)?;
                Value::Str(self.heap_mut().alloc_string(text))
            }
            Io::ReadBinary => {
                let bytes = self.heap_mut().storage()?.read_binary(&name, &mode)?;
                Value::Octet(self.heap_mut().alloc_octet(bytes))
            }
            Io::WriteText(text) => {
                self.heap_mut().storage()?.write_text(&name, &mode, &text)?;
                Value::Void
            }
            Io::WriteBinary(bytes) => {
                self.heap_mut()
                    .storage()?
                    .write_binary(&name, &mode, &bytes)?;
                Value::Void
            }
        };
        next.resume(self, value)
    }
}
impl Heap {
    pub fn set_storage(&mut self, storage: impl Storage + 'static) {
        self.storage = Some(Box::new(storage));
    }
    pub fn storage(&mut self) -> NativeResult<&mut (dyn Storage + 'static)> {
        self.storage
            .as_deref_mut()
            .ok_or(NativeError::Message("host storage is not installed"))
    }
}
