use crate::setup::setup_write;
use crate::txbuf::TxBuf;
use ethercrab::{
    MainDevice, PduHeader, SubDevice, error::Error, received_frame::ReceivedPdu, std::RawSocketDesc,
};
use io_uring::{IoUring, types::Timespec};
use std::collections::BTreeMap;

use heapless::Deque;

pub struct Dc<const N: usize> {
    subdevices: Deque<SubDevice, N>,
    state: DcState,
}

enum DcState {
    LatchReceive,
    LatchTimes {
        supported_devices: u16,
        attr_count: u16,
    },
    Configure {
        master_dc: Option<usize>,
        supported_devices: u16,
        attr_count: u16,
    },
    StaticSync {
        master_dc: usize,
        remaining_iterations: u32,
    },
}

impl<const N: usize> Dc<N> {
    pub(crate) fn new(subdevices: Deque<SubDevice, N>) -> Self {
        Self {
            subdevices,
            state: DcState::LatchReceive,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start(
        &mut self,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut IoUring,
        write_entry: impl Fn(u64) -> u64,
    ) -> Result<(), Error> {
        let (frame, handle) = maindevice.prep_latch_receive_times().unwrap().unwrap();

        setup_write(
            frame,
            handle,
            retry_count,
            timeout,
            tx_entries,
            sock,
            ring,
            None,
            None,
            write_entry,
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn update(
        &mut self,
        received: ReceivedPdu<'_>,
        header: PduHeader,
        maindevice: &mut MainDevice,
        retry_count: usize,
        timeout: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut io_uring::IoUring,
        idx: Option<u16>,
        write_entry: impl Fn(u64) -> u64,
    ) -> Result<Option<Deque<SubDevice, N>>, Error> {
        match &mut self.state {
            DcState::LatchReceive => {
                if header.command_code != 8 {
                    unreachable!()
                }

                let dc_supported_devices = maindevice
                    .prep_latch_dc_times(self.subdevices.as_mut_slices().0, |res, id| {
                        let (frame, handle) = res.unwrap().unwrap();

                        setup_write(
                            frame,
                            handle,
                            retry_count,
                            timeout,
                            tx_entries,
                            sock,
                            ring,
                            Some(id),
                            None,
                            &write_entry,
                        )?;
                        Ok(())
                    })
                    .unwrap();

                self.state = DcState::LatchTimes {
                    supported_devices: dc_supported_devices,
                    attr_count: 0,
                }
            }
            DcState::LatchTimes {
                supported_devices,
                attr_count,
            } => {
                assert_eq!(header.command_code, 4);

                let addr = (u32::from_le_bytes(header.command_raw) >> 16) as u16;
                let id = idx.unwrap() as usize;

                use ethercrab::EtherCrabWireRead;
                match (self.subdevices.get_mut(id), addr) {
                    // dc receive time
                    (Some(dev), 0x0918) => {
                        let bytes = &*received;
                        let dc_receive_time = u64::unpack_from_slice(bytes).unwrap();
                        dev.set_dc_receive_time(dc_receive_time)
                    }
                    // dc time port 0
                    (Some(dev), 0x0900) => {
                        let bytes = &*received;
                        let [time_p0, time_p1, time_p2, time_p3] =
                            <[u32; 4]>::unpack_from_slice(bytes).unwrap();
                        dev.ports
                            .set_receive_times(time_p0, time_p3, time_p1, time_p2);
                    }
                    _ => unreachable!(),
                }

                *attr_count += 1;

                if *attr_count != *supported_devices * 2 {
                    return Ok(None);
                }

                let master_dc_device = maindevice
                    .prep_configure_dc(
                        self.subdevices.as_mut_slices().0,
                        ethercrab::std::ethercat_now,
                        |res, id| {
                            let (frame, handle) = res.unwrap().unwrap();

                            setup_write(
                                frame,
                                handle,
                                retry_count,
                                timeout,
                                tx_entries,
                                sock,
                                ring,
                                Some(id),
                                None,
                                &write_entry,
                            )?;
                            Ok(())
                        },
                    )
                    .unwrap();

                self.state = DcState::Configure {
                    supported_devices: *supported_devices,
                    attr_count: 0,
                    master_dc: master_dc_device,
                }
            }
            DcState::Configure {
                supported_devices,
                master_dc,
                attr_count,
            } => {
                assert_eq!(header.command_code, 5);

                let addr = (u32::from_le_bytes(header.command_raw) >> 16) as u16;
                // write cmds so theres nothing to read
                if !matches!(addr, 0x0920 | 0x0928) {
                    unreachable!()
                }

                *attr_count += 1;

                if *attr_count != *supported_devices * 2 {
                    return Ok(None);
                }

                if let Some(master_idx) = master_dc {
                    let master = self.subdevices.get(*master_idx).unwrap();

                    maindevice.dc_reference_configured_address.store(
                        master.configured_address(),
                        std::sync::atomic::Ordering::Relaxed,
                    );

                    let (frame, handle) = maindevice.prep_dc_static_sync(master).unwrap().unwrap();
                    setup_write(
                        frame,
                        handle,
                        retry_count,
                        timeout,
                        tx_entries,
                        sock,
                        ring,
                        None,
                        None,
                        &write_entry,
                    )?;

                    let remaining_iterations = maindevice.config.dc_static_sync_iterations;

                    self.state = DcState::StaticSync {
                        master_dc: *master_idx,
                        remaining_iterations,
                    }
                } else {
                    todo!("non dc supported devices");
                }
            }
            DcState::StaticSync {
                master_dc,
                remaining_iterations,
            } => {
                assert_eq!(header.command_code, 14);

                if *remaining_iterations > 0 {
                    *remaining_iterations -= 1;
                    let master = self.subdevices.get(*master_dc).unwrap();

                    let (frame, handle) = maindevice.prep_dc_static_sync(master).unwrap().unwrap();
                    setup_write(
                        frame,
                        handle,
                        retry_count,
                        timeout,
                        tx_entries,
                        sock,
                        ring,
                        None,
                        None,
                        &write_entry,
                    )?;
                } else {
                    return Ok(Some(core::mem::take(&mut self.subdevices)));
                }
            }
        }
        Ok(None)
    }
}
