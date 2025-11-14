#[non_exhaustive]
pub enum ControlFlow {
    Send,
}

pub trait UserDevice {
    fn subdevice_mut(&mut self) -> &mut ethercrab::SubDevice;
    fn subdevice(&self) -> &ethercrab::SubDevice;
    fn into_subdevice(self) -> ethercrab::SubDevice;
}
