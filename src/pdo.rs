use crate::pdo_config::{PdoMap, PdoMapState};
use crate::sdo::SdoWrite;

pub struct PdoConfig<'a, const I: usize, const O: usize> {
    pub inputs: [PdoMapping<'a>; I],
    pub outputs: [PdoMapping<'a>; O],
}

impl<'a, const I: usize, const O: usize> PdoConfig<'a, I, O> {
    pub fn new(inputs: [PdoMapping<'a>; I], outputs: [PdoMapping<'a>; O]) -> Self {
        Self { inputs, outputs }
    }
}

pub struct PdoMapping<'a> {
    pub(crate) index: u16,
    pub(crate) objects: &'a [PdoObject],
}

impl<'a> PdoMapping<'a> {
    pub fn new(index: u16, objects: &'a [PdoObject]) -> Self {
        Self { index, objects }
    }

    pub fn len_bytes(&self) -> u16 {
        self.objects.iter().map(|obj| obj.len_bytes()).sum()
    }

    pub(crate) fn start_map(&'a self, subdev: &ethercrab::SubDevice) -> PdoMap<'a> {
        PdoMap {
            index: self.index,
            objects: self.objects,
            state: PdoMapState::Clear(SdoWrite::new(subdev, self.index, 0, 0)),
        }
    }
}

#[derive(Debug)]
pub struct PdoObject(pub(crate) u32);

impl PdoObject {
    pub const fn new<T: ethercrab::EtherCrabWireSized>(index: u16, subindex: u8) -> Self {
        Self((index as u32) << 16 | (subindex as u32) << 8 | ((T::PACKED_LEN as u32 * 8) & 0xFF))
    }

    pub const fn len_bytes(&self) -> u16 {
        // lower 8 bits are object size
        let bits = (self.0 & 0xFF) as u16;
        // round to nearest byte
        bits.div_ceil(8)
    }
}
