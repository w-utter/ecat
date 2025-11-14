#[non_exhaustive]
#[derive(Clone, Copy)]
pub enum ControlFlow {
    Restart,
}

pub trait UserDevice {
    fn subdevice_mut(&mut self) -> &mut ethercrab::SubDevice;
    fn subdevice(&self) -> &ethercrab::SubDevice;
    fn into_subdevice(self) -> ethercrab::SubDevice;
}
