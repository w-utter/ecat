use crate::setup::setup_write;
use crate::txbuf::TxBuf;
use ethercrab::{
    EtherCrabWireSized, MainDevice, PduHeader, SubDevice, error::Error,
    received_frame::ReceivedPdu, std::RawSocketDesc,
};
use io_uring::{IoUring, types::Timespec};
use std::collections::BTreeMap;

use crate::eeprom::category::CategoryIter;

use crate::pdo::PdoConfig;
use crate::preop::FmmuMapping as FmmuMappingOutput;

#[allow(clippy::large_enum_variant)]
pub enum ConfigureFmmus {
    SyncManagers(
        CategoryIter<{ ethercrab::SyncManager::PACKED_LEN }>,
        heapless::Vec<ethercrab::SyncManager, 8>,
    ),
    Mappings(
        heapless::Vec<ethercrab::SyncManager, 8>,
        CategoryIter<{ ethercrab::FmmuUsage::PACKED_LEN }>,
        heapless::Vec<ethercrab::FmmuUsage, 16>,
    ),
    Configure {
        input_iter: heapless::index_map::IntoIter<u8, FmmuMapping, 16>,
        output_iter: heapless::index_map::IntoIter<u8, FmmuMapping, 16>,
        current_input: Option<ConfigureFmmu>,
        current_output: Option<ConfigureFmmu>,
        input_len: Option<usize>,
        output_len: Option<usize>,
    },
}

impl ConfigureFmmus {
    pub(crate) fn new() -> Self {
        Self::SyncManagers(
            CategoryIter::new(ethercrab::CategoryType::SyncManager, 0),
            Default::default(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start(
        &mut self,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout_duration: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        configured_addr: u16,
        idx: u16,
        write_entry: impl Fn(u64) -> u64,
    ) {
        match self {
            Self::SyncManagers(s, _) => {
                let _ = s.start(
                    maindevice,
                    retry_count,
                    timeout_duration,
                    tx_entries,
                    sock,
                    ring,
                    configured_addr,
                    idx,
                    write_entry,
                );
            }
            _ => unreachable!(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_output(
        &mut self,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout_duration: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        configured_addr: u16,
        idx: u16,
        write_entry: impl Fn(u64) -> u64,
    ) -> Result<(), Error> {
        match self {
            Self::Configure {
                output_iter,
                current_output,
                ..
            } => {
                *current_output = output_iter
                    .next()
                    .map(|(sm_idx, mapping)| {
                        ConfigureFmmu::start_new(
                            sm_idx,
                            mapping,
                            ethercrab::SyncManagerType::ProcessDataWrite,
                            maindevice,
                            retry_count,
                            timeout_duration,
                            tx_entries,
                            sock,
                            ring,
                            configured_addr,
                            Some(2),
                            idx,
                            write_entry,
                        )
                    })
                    .transpose()?;
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn update<const I: usize, const O: usize>(
        &mut self,
        received: ReceivedPdu<'_>,
        header: PduHeader,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout_duration: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        configured_addr: u16,
        idx: u16,
        subdev: &mut ethercrab::SubDevice,
        identifier: Option<u8>,
        config: &PdoConfig<'_, I, O>,
        pdi_offset: &mut ethercrab::PdiOffset,
        write_entry: impl Fn(u64) -> u64,
    ) -> Result<Option<FmmuMappingOutput<(usize, usize)>>, Error> {
        match self {
            Self::SyncManagers(managers, collected) => {
                if let Some(more) = managers.update(
                    received,
                    header,
                    maindevice,
                    retry_count,
                    timeout_duration,
                    tx_entries,
                    sock,
                    ring,
                    configured_addr,
                    idx,
                    &write_entry,
                )? {
                    if let Some(buf) = managers.buffer() {
                        use ethercrab::EtherCrabWireRead;
                        let mgr = ethercrab::SyncManager::unpack_from_slice(buf).unwrap();

                        let _ = collected.push(mgr);
                    } else {
                        panic!("could not find sync manager")
                    }

                    if !more {
                        let mut fmmus = CategoryIter::new(ethercrab::CategoryType::Fmmu, 0);
                        fmmus.start(
                            maindevice,
                            retry_count,
                            timeout_duration,
                            tx_entries,
                            sock,
                            ring,
                            configured_addr,
                            idx,
                            &write_entry,
                        )?;

                        *self =
                            Self::Mappings(core::mem::take(collected), fmmus, Default::default());
                    }
                }
            }
            Self::Mappings(managers, fmmus, collected) => {
                if let Some(more) = fmmus.update(
                    received,
                    header,
                    maindevice,
                    retry_count,
                    timeout_duration,
                    tx_entries,
                    sock,
                    ring,
                    configured_addr,
                    idx,
                    &write_entry,
                )? {
                    if let Some(buf) = fmmus.buffer() {
                        use ethercrab::EtherCrabWireRead;
                        let fmmu = ethercrab::FmmuUsage::unpack_from_slice(buf).unwrap();

                        let _ = collected.push(fmmu);
                    } else {
                        panic!("could not find sync manager")
                    }

                    if !more {
                        let inputs = FmmuMapping::from_config(
                            config,
                            ethercrab::PdoDirection::MasterRead,
                            managers,
                            collected,
                        );
                        let outputs = FmmuMapping::from_config(
                            config,
                            ethercrab::PdoDirection::MasterWrite,
                            managers,
                            collected,
                        );

                        let mut input_iter = inputs.into_iter();
                        let output_iter = outputs.into_iter();

                        let current_input = input_iter
                            .next()
                            .map(|(sm_idx, mapping)| {
                                ConfigureFmmu::start_new(
                                    sm_idx,
                                    mapping,
                                    ethercrab::SyncManagerType::ProcessDataRead,
                                    maindevice,
                                    retry_count,
                                    timeout_duration,
                                    tx_entries,
                                    sock,
                                    ring,
                                    configured_addr,
                                    Some(1),
                                    idx,
                                    &write_entry,
                                )
                            })
                            .transpose()?;

                        *self = Self::Configure {
                            input_iter,
                            output_iter,
                            current_input,
                            current_output: None,
                            input_len: None,
                            output_len: None,
                        };
                    }
                }
            }
            Self::Configure {
                input_iter,
                output_iter,
                current_input,
                current_output,
                input_len,
                output_len,
            } => {
                match identifier.map(|id| (id >> 2) & 0b11) {
                    Some(1) => {
                        let input = current_input.as_mut().unwrap();

                        if input.update(
                            received,
                            header,
                            maindevice,
                            retry_count,
                            timeout_duration,
                            tx_entries,
                            sock,
                            ring,
                            configured_addr,
                            identifier,
                            idx,
                            pdi_offset,
                            subdev,
                            ethercrab::PdoDirection::MasterRead,
                            &write_entry,
                        )? {
                            *current_input = input_iter
                                .next()
                                .map(|(sm_idx, mapping)| {
                                    ConfigureFmmu::start_new(
                                        sm_idx,
                                        mapping,
                                        ethercrab::SyncManagerType::ProcessDataRead,
                                        maindevice,
                                        retry_count,
                                        timeout_duration,
                                        tx_entries,
                                        sock,
                                        ring,
                                        configured_addr,
                                        Some(1),
                                        idx,
                                        &write_entry,
                                    )
                                })
                                .transpose()?;

                            // doing this sequentially again
                            // due to needing sequential access to the pdi
                            if current_input.is_none() {
                                *input_len = Some(pdi_offset.start_address as _);
                                /*
                                *current_output = output_iter
                                    .next()
                                    .map(|(sm_idx, mapping)| {
                                        ConfigureFmmu::start_new(
                                            sm_idx,
                                            mapping,
                                            ethercrab::SyncManagerType::ProcessDataWrite,
                                            maindevice,
                                            retry_count,
                                            timeout_duration,
                                            tx_entries,
                                            sock,
                                            ring,
                                            configured_addr,
                                            Some(2),
                                            idx,
                                        )
                                    })
                                    .transpose()?;
                                */
                            }
                            return Ok(Some(FmmuMappingOutput::Input));

                            /*
                            if current_input.is_none() && current_output.is_none() {
                                return Ok(Some(FmmuMappingOutput::Output((
                                    input_len.unwrap_or_default(),
                                    output_len.unwrap_or_default(),
                                ))));
                            } else {
                            }
                            */
                        }
                    }
                    Some(2) => {
                        let output = current_output.as_mut().unwrap();

                        if output.update(
                            received,
                            header,
                            maindevice,
                            retry_count,
                            timeout_duration,
                            tx_entries,
                            sock,
                            ring,
                            configured_addr,
                            identifier,
                            idx,
                            pdi_offset,
                            subdev,
                            ethercrab::PdoDirection::MasterWrite,
                            &write_entry,
                        )? {
                            *current_output = output_iter
                                .next()
                                .map(|(sm_idx, mapping)| {
                                    ConfigureFmmu::start_new(
                                        sm_idx,
                                        mapping,
                                        ethercrab::SyncManagerType::ProcessDataWrite,
                                        maindevice,
                                        retry_count,
                                        timeout_duration,
                                        tx_entries,
                                        sock,
                                        ring,
                                        configured_addr,
                                        Some(2),
                                        idx,
                                        &write_entry,
                                    )
                                })
                                .transpose()?;

                            if current_output.is_none() {
                                *output_len = Some(pdi_offset.start_address as _);
                            }

                            if current_input.is_none() && current_output.is_none() {
                                return Ok(Some(FmmuMappingOutput::Output((
                                    input_len.unwrap_or_default(),
                                    output_len.unwrap_or_default(),
                                ))));
                            }
                        }
                    }
                    _ => unreachable!(),
                }
            }
        }
        Ok(None)
    }
}

#[derive(Debug)]
pub struct FmmuMapping {
    sync_manager: ethercrab::SyncManager,
    fmmu_index: u8,
    length: u16,
}

impl FmmuMapping {
    fn from_config<const I: usize, const O: usize, const N: usize>(
        config: &PdoConfig<'_, I, O>,
        direction: ethercrab::PdoDirection,
        sync_managers: &[ethercrab::SyncManager],
        fmmus: &[ethercrab::FmmuUsage],
    ) -> heapless::index_map::FnvIndexMap<u8, Self, N> {
        use ethercrab::{PdoDirection, SyncManagerType};
        let objects = match direction {
            PdoDirection::MasterRead => config.inputs.as_slice(),
            PdoDirection::MasterWrite => config.outputs.as_slice(),
        };

        let ty = match direction {
            PdoDirection::MasterRead => SyncManagerType::ProcessDataRead,
            PdoDirection::MasterWrite => SyncManagerType::ProcessDataWrite,
        };

        let fmmu_usage = match direction {
            PdoDirection::MasterRead => ethercrab::FmmuUsage::Inputs,
            PdoDirection::MasterWrite => ethercrab::FmmuUsage::Outputs,
        };

        use heapless::index_map::{Entry, FnvIndexMap, IndexMap};
        let mut config: IndexMap<u8, Self, _, N> = FnvIndexMap::new();

        for assignment in objects.iter() {
            let (sync_manager_idx, sync_manager) = sync_managers
                .iter()
                .enumerate()
                .find(|(_, sm)| sm.usage_type() == ty)
                .map(|(idx, sm)| (idx as u8, sm))
                .unwrap();

            let fmmu_index = fmmus
                .iter()
                .position(|&usage| usage == fmmu_usage)
                .map(|pos| pos as u8)
                .unwrap();

            let len = assignment.len_bytes();

            match config.entry(sync_manager_idx) {
                Entry::Occupied(mut cfg) => {
                    cfg.get_mut().length += len;
                }
                Entry::Vacant(entry) => {
                    let sync_manager = *sync_manager;
                    let _ = entry.insert(Self {
                        sync_manager,
                        fmmu_index,
                        length: len,
                    });
                }
            }
        }
        config
    }
}

pub struct ConfigureFmmu {
    receive_sync_manager: bool,
    receive_fmmu: bool,
    fmmu: FmmuConfig,
}

impl ConfigureFmmu {
    fn from_fmmu(fmmu: FmmuConfig) -> Self {
        Self {
            receive_sync_manager: false,
            receive_fmmu: false,
            fmmu,
        }
    }

    /// both creates and starts self
    /// - this is because the fmmu instantiation depends on the config given from writing to the sync manager
    #[allow(clippy::too_many_arguments)]
    fn start_new(
        sm_idx: u8,
        mapping: FmmuMapping,
        sm_type: ethercrab::SyncManagerType,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout_duration: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        configured_addr: u16,
        identifier: Option<u8>,
        idx: u16,
        write_entry: impl Fn(u64) -> u64,
    ) -> Result<Self, Error> {
        let ((frame, handle), config) = maindevice
            .prep_write_sm_config(
                configured_addr,
                sm_idx,
                &mapping.sync_manager,
                mapping.length,
            )
            .unwrap()
            .unwrap();
        setup_write(
            frame,
            handle,
            retry_count,
            timeout_duration,
            tx_entries,
            sock,
            ring,
            Some(idx),
            Some(1 | (identifier.unwrap_or(0) << 2)),
            &write_entry,
        )?;

        let mut fmmu = FmmuConfig::new(mapping.fmmu_index, sm_type, &config);
        fmmu.start(
            maindevice,
            retry_count,
            timeout_duration,
            tx_entries,
            sock,
            ring,
            configured_addr,
            Some(2 | (identifier.unwrap_or(0) << 2)),
            idx,
            write_entry,
        )?;

        Ok(Self::from_fmmu(fmmu))
    }

    #[allow(clippy::too_many_arguments)]
    fn update(
        &mut self,
        received: ReceivedPdu<'_>,
        header: PduHeader,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout_duration: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        configured_addr: u16,
        identifier: Option<u8>,
        idx: u16,
        pdi_offset: &mut ethercrab::PdiOffset,
        subdev: &mut SubDevice,
        direction: ethercrab::PdoDirection,
        write_entry: impl Fn(u64) -> u64,
    ) -> Result<bool, Error> {
        // first 2 bits to identify
        match identifier.map(|id| id & 0b11) {
            Some(1) => {
                self.receive_sync_manager = true;
                if self.receive_fmmu {
                    return Ok(true);
                }
            }
            Some(2) => {
                if let Some(segment) = self.fmmu.update(
                    received,
                    header,
                    maindevice,
                    retry_count,
                    timeout_duration,
                    tx_entries,
                    sock,
                    ring,
                    configured_addr,
                    identifier,
                    idx,
                    pdi_offset,
                    write_entry,
                )? {
                    use ethercrab::PdoDirection;
                    match direction {
                        PdoDirection::MasterRead => subdev.config.io.input = segment,
                        PdoDirection::MasterWrite => subdev.config.io.output = segment,
                    }

                    self.receive_fmmu = true;
                    if self.receive_sync_manager {
                        return Ok(true);
                    }
                }
            }
            _ => unreachable!(),
        }
        Ok(false)
    }
}

enum FmmuConfig {
    ReadFmmu {
        fmmu_idx: u8,
        sm_type: ethercrab::SyncManagerType,
        sm_length_bytes: u16,
        sm_physical_start_addr: u16,
    },
    WriteConfig(ethercrab::Fmmu, u16, u8),
    CheckFmmu(ethercrab::PdiSegment),
}

impl FmmuConfig {
    fn new(
        fmmu_idx: u8,
        desired_type: ethercrab::SyncManagerType,
        config: &ethercrab::sync_manager_channel::SyncManagerChannel,
    ) -> Self {
        Self::ReadFmmu {
            fmmu_idx,
            sm_type: desired_type,
            sm_length_bytes: config.length_bytes,
            sm_physical_start_addr: config.physical_start_address,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout_duration: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        configured_addr: u16,
        identifier: Option<u8>,
        idx: u16,
        write_entry: impl Fn(u64) -> u64,
    ) -> Result<(), Error> {
        match self {
            Self::ReadFmmu { fmmu_idx, .. } => {
                let (frame, handle) = maindevice
                    .prep_read_fmmu(configured_addr, *fmmu_idx)
                    .unwrap()
                    .unwrap();

                setup_write(
                    frame,
                    handle,
                    retry_count,
                    timeout_duration,
                    tx_entries,
                    sock,
                    ring,
                    Some(idx),
                    identifier,
                    write_entry,
                )?;
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn update(
        &mut self,
        received: ReceivedPdu<'_>,
        _header: PduHeader,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout_duration: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        configured_addr: u16,
        identifier: Option<u8>,
        idx: u16,
        pdi_offset: &mut ethercrab::PdiOffset,
        write_entry: impl Fn(u64) -> u64,
    ) -> Result<Option<ethercrab::PdiSegment>, Error> {
        match self {
            Self::ReadFmmu {
                fmmu_idx,
                sm_type,
                sm_length_bytes,
                sm_physical_start_addr,
            } => {
                use ethercrab::EtherCrabWireRead;
                let mut fmmu = ethercrab::Fmmu::unpack_from_slice(&received).unwrap();

                let fmmu = if fmmu.enable {
                    fmmu.length_bytes += *sm_length_bytes;
                    fmmu
                } else {
                    ethercrab::Fmmu {
                        logical_start_address: pdi_offset.start_address,
                        length_bytes: *sm_length_bytes,
                        logical_start_bit: 0,
                        logical_end_bit: 7,
                        physical_start_address: *sm_physical_start_addr,
                        physical_start_bit: 0x0,
                        read_enable: matches!(sm_type, ethercrab::SyncManagerType::ProcessDataRead),
                        write_enable: matches!(
                            sm_type,
                            ethercrab::SyncManagerType::ProcessDataWrite
                        ),
                        enable: true,
                    }
                };

                let (frame, handle) = maindevice
                    .prep_write_fmmu(configured_addr, *fmmu_idx, fmmu)
                    .unwrap()
                    .unwrap();

                setup_write(
                    frame,
                    handle,
                    retry_count,
                    timeout_duration,
                    tx_entries,
                    sock,
                    ring,
                    Some(idx),
                    identifier,
                    write_entry,
                )?;
                *self = Self::WriteConfig(fmmu, *sm_length_bytes, *fmmu_idx);
            }
            Self::WriteConfig(_fmmu, len, fmmu_idx) => {
                let starting_pdi = *pdi_offset;
                *pdi_offset = pdi_offset.increment(*len);

                let segment = ethercrab::PdiSegment {
                    bytes: starting_pdi.up_to(*pdi_offset),
                };

                let (frame, handle) = maindevice
                    .prep_read_fmmu(configured_addr, *fmmu_idx)
                    .unwrap()
                    .unwrap();

                setup_write(
                    frame,
                    handle,
                    retry_count,
                    timeout_duration,
                    tx_entries,
                    sock,
                    ring,
                    Some(idx),
                    identifier,
                    write_entry,
                )?;

                *self = Self::CheckFmmu(segment);
            }
            Self::CheckFmmu(segment) => {
                use ethercrab::EtherCrabWireRead;
                let _fmmu = ethercrab::Fmmu::unpack_from_slice(&received).unwrap();

                return Ok(Some(segment.clone()));
            }
        }
        Ok(None)
    }
}
