use ecat::{
    InitState, PdoConfig, PdoMapping, PdoObject, SdoRead, SdoWrite, TxBuf, TxIndex,
    user::ControlFlow,
};
use ethercrab::{SubDevice, error::Error};

use ecat::io::{TIMEOUT_CLEAR_MASK, TIMEOUT_MASK, WRITE_MASK};

use io_uring::types::Timespec;

const MAX_PDU_DATA: usize = ethercrab::PduStorage::element_size(1100);
const MAX_FRAMES: usize = 64;

static PDU_STORAGE: ethercrab::PduStorage<MAX_FRAMES, MAX_PDU_DATA> = ethercrab::PduStorage::new();

use std::collections::BTreeMap;

use ethercrab::MainDevice;
use ethercrab::std::RawSocketDesc;

fn setup_process_priority() -> Result<(), Box<dyn std::error::Error>> {
    use thread_priority::*;
    let this = std::thread::current();
    this.set_priority_and_policy(
        ThreadSchedulePolicy::Realtime(RealtimeThreadSchedulePolicy::Fifo),
        ThreadPriority::Crossplatform(ThreadPriorityValue::try_from(49).unwrap()),
    )?;

    for core_id in core_affinity::get_core_ids().unwrap() {
        if core_affinity::set_for_current(core_id) {
            break;
        }
    }

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    setup_process_priority()?;

    let (_tx, mut rx, pdu_loop) = PDU_STORAGE.try_split().expect("cannot split pdu");

    let mut maindevice = MainDevice::new(
        pdu_loop,
        ethercrab::Timeouts::default(),
        ethercrab::MainDeviceConfig {
            dc_static_sync_iterations: 1000,
            ..Default::default()
        },
    );

    let mut ring = io_uring::IoUring::new(64)?;

    let mut probe = io_uring::register::Probe::new();
    ring.submitter().register_probe(&mut probe)?;

    use io_uring::{opcode, types};

    if !probe.is_supported(opcode::RecvMulti::CODE) || !probe.is_supported(opcode::Write::CODE) {
        panic!("readmulti/write opcodes are not supported");
    }

    let mut sock = RawSocketDesc::new("enxf8edfcadd5c6")?;
    //let mut sock = RawSocketDesc::new("enxf8e43bcd2609")?;
    let mtu = sock.interface_mtu()?;

    let mtu = mtu + 18;
    const ENTRIES: usize = 256;

    let mut tx_bufs = BTreeMap::new();

    let idx = 0;

    let mut rx_bufs = io_uring_buf_ring::BufRing::new(ENTRIES as _, mtu as _, idx)?
        .register(&ring.submitter())
        .map_err(|(err, _)| err)?
        .init();

    use std::os::fd::AsRawFd;
    let rx_multi_entry =
        opcode::RecvMulti::new(types::Fd(sock.as_raw_fd()), rx_bufs.bgid()).build();

    while unsafe { ring.submission().push(&rx_multi_entry).is_err() } {
        ring.submit().expect("could not submit ops");
    }
    ring.submit().expect("could not submit ops");

    let retries = 5;

    //TODO: update to actual timeout duration.
    let timeout = Timespec::new().sec(1);

    let mut state = InitState::<16, _, _, _>::new();

    let write_entry = |id| id | WRITE_MASK;
    let timeout_entry = |id| id | TIMEOUT_MASK;

    state.start(
        &maindevice,
        retries,
        &timeout,
        &mut tx_bufs,
        &sock,
        &mut ring,
        &write_entry,
        &timeout_entry,
    )?;

    let mut pdi_offset = ethercrab::PdiOffset::default();

    let config = PdoConfig::new(
        // inputs
        [PdoMapping::new(
            0x1A00,
            const {
                &[
                    // status word
                    PdoObject::new::<u16>(0x6041, 0),
                    // actual position
                    PdoObject::new::<u32>(0x6064, 0),
                    // actual velocity
                    PdoObject::new::<u32>(0x606C, 0),
                    // actual torque
                    PdoObject::new::<u16>(0x6077, 0),
                ]
            },
        )],
        [
            // outputs
            PdoMapping::new(
                0x1600,
                const {
                    &[
                        // control word
                        PdoObject::new::<u16>(0x6040, 0),
                        // target velocity
                        PdoObject::new::<u32>(0x60FF, 0),
                        // op mode
                        PdoObject::new::<u8>(0x6060, 0),
                        // padding
                        PdoObject::new::<u8>(0, 0),
                    ]
                },
            ),
        ],
    );

    loop {
        let cqueue_entry = ring.completion().next();
        if let Some(entry) = cqueue_entry {
            let udata = entry.user_data();
            if udata & TIMEOUT_CLEAR_MASK == TIMEOUT_CLEAR_MASK {
                let key = udata & 0xFFFFFF;
                let res = tx_bufs.remove(&key).expect("could not get received entry");

                if let Some((header, pdu)) = res.received {
                    let _ = state.update(
                        pdu,
                        header,
                        &mut maindevice,
                        retries,
                        &timeout,
                        &mut tx_bufs,
                        &sock,
                        &mut ring,
                        res.configured_addr,
                        res.identifier,
                        &mut pdi_offset,
                        |_maindev, subdev| (User::new(subdev), &config),
                        |maindev, dev, received, entries, ring, index, identifier, output_buf| {
                            let flow = dev
                                .update(
                                    received,
                                    maindev,
                                    retries,
                                    &timeout,
                                    entries,
                                    &sock,
                                    ring,
                                    index,
                                    identifier,
                                    output_buf,
                                    &write_entry,
                                    &timeout_entry,
                                )
                                .unwrap();

                            Ok(flow)
                        },
                        &write_entry,
                        &timeout_entry,
                    );
                } else {
                    println!("actually timed out");
                }
                continue;
            } else if udata & WRITE_MASK == WRITE_MASK {
                continue;
            } else if udata & TIMEOUT_MASK == TIMEOUT_MASK {
                if !matches!(-entry.result(), libc::ECANCELED) {
                    let key = entry.user_data() & 0xFFFFFF;

                    let Some(entry) = tx_bufs.get_mut(&key) else {
                        println!("could not find entry :(");
                        continue;
                    };

                    println!("timeout");

                    if entry.retries_remaining == 0 {
                        let timeout_clear =
                            io_uring::opcode::TimeoutRemove::new(key | TIMEOUT_MASK)
                                .build()
                                .user_data(key | TIMEOUT_CLEAR_MASK);

                        while unsafe { ring.submission().push(&timeout_clear).is_err() } {
                            ring.submit().expect("could not submit ops");
                        }
                        ring.submit().unwrap();
                    } else {
                        while unsafe { ring.submission().push(entry.entry()).is_err() } {
                            ring.submit().expect("could not submit ops");
                        }
                        ring.submit().unwrap();
                        entry.retries_remaining -= 1;
                    }
                }
                continue;
            }

            let Ok(Some(id)) = rx_bufs.buffer_id_from_cqe(&entry) else {
                continue;
            };

            let buf = id.buffer();

            let Some(recv_frame) = rx.receive_frame_io_uring(buf).unwrap() else {
                continue;
            };

            let frame: ethercrab::received_frame::ReceivedFrame = recv_frame.into();

            for (idx, res) in frame.into_pdu_iter_with_headers().enumerate() {
                let Ok((pdu, header)) = res else {
                    continue;
                };
                let rx_idx = (idx, header).idx();

                if let Some(entry) = tx_bufs.get_mut(&rx_idx) {
                    entry.received = Some((header, pdu));

                    let timeout_clear = io_uring::opcode::TimeoutRemove::new(rx_idx | TIMEOUT_MASK)
                        .build()
                        .user_data(rx_idx | TIMEOUT_CLEAR_MASK);

                    while unsafe { ring.submission().push(&timeout_clear).is_err() } {
                        ring.submit().expect("could not submit ops");
                    }

                    ring.submit().unwrap();
                }
            }
        }
    }
}

struct User {
    device: ethercrab::SubDevice,
    state: UserState,
}

impl User {
    fn new(device: ethercrab::SubDevice) -> Self {
        Self {
            device,
            state: Default::default(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn update(
        &mut self,
        received: Option<ecat::DeviceResponse<'_, '_>>,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout_duration: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut io_uring::IoUring,
        idx: u16,
        identifier: Option<u8>,
        output_buf: &mut [u8],
        write_entry: impl Fn(u64) -> u64,
        timeout_entry: impl Fn(u64) -> u64,
    ) -> Result<Option<ControlFlow>, Error> {
        self.state.update(
            received,
            maindevice,
            retry_count,
            timeout_duration,
            tx_entries,
            sock,
            ring,
            &mut self.device,
            idx,
            identifier,
            output_buf,
            write_entry,
            timeout_entry,
        )
    }
}

impl ecat::user::UserDevice for User {
    fn subdevice(&self) -> &ethercrab::SubDevice {
        &self.device
    }

    fn subdevice_mut(&mut self) -> &mut ethercrab::SubDevice {
        &mut self.device
    }

    fn into_subdevice(self) -> ethercrab::SubDevice {
        self.device
    }
}

#[derive(Default)]
enum UserState {
    #[default]
    Idle,
    //Init(UserControlInit),
    Test(u32),
    Error(SdoRead<u16>),
}

pub enum UserControlInitState {
    // | operation mode related parameters
    MaxVelocity(SdoWrite<u32>),
    MaxAcceleration(SdoWrite<u32>),
    QuickStopDecel(SdoWrite<u32>),
    ProfileDecel(SdoWrite<u32>),
    // |
}

#[derive(Copy, Clone, ethercrab_wire::EtherCrabWireRead)]
#[wire(bytes = 12)]
struct RecvObj {
    #[wire(bytes = 2)]
    status: u16,
    #[wire(bytes = 4)]
    position: i32,
    #[wire(bytes = 4)]
    velocity: i32,
    #[wire(bytes = 2)]
    torque: i16,
}

impl RecvObj {
    const ERR_MASK: u16 = 0x08;
}

#[derive(Copy, Clone, Debug, ethercrab_wire::EtherCrabWireWrite)]
#[wire(bytes = 8)]
struct WriteObj {
    #[wire(bytes = 2)]
    control: u16,
    #[wire(bytes = 4)]
    target_velocity: i32,
    #[wire(bytes = 1)]
    opmode: u8,
    #[wire(bytes = 1)]
    padding: u8,
}

impl WriteObj {
    fn new(control: u16, target_velocity: i32, opmode: u8) -> Self {
        Self {
            control,
            target_velocity,
            opmode,
            padding: 0,
        }
    }
}

impl core::fmt::Debug for RecvObj {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let cvp = CVP {
            position: self.position,
            velocity: self.velocity,
            torque: self.torque,
        };

        write!(f, "(0x{:02x}, {cvp:?})", self.status)
    }
}

impl UserState {
    #[allow(clippy::too_many_arguments)]
    fn update(
        &mut self,
        received: Option<ecat::DeviceResponse<'_, '_>>,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout_duration: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut io_uring::IoUring,
        subdev: &mut SubDevice,
        idx: u16,
        identifier: Option<u8>,
        output_buf: &mut [u8],
        write_entry: impl Fn(u64) -> u64,
        timeout_entry: impl Fn(u64) -> u64,
    ) -> Result<Option<ControlFlow>, Error> {
        match self {
            Self::Idle => {
                println!("attempting to start rx/tx");
                use ethercrab::EtherCrabWireWrite;
                let write_obj = WriteObj::new(0x0080, 0, 9);
                write_obj.pack_to_slice(output_buf).unwrap();

                println!("send: {output_buf:?}");

                *self = Self::Test(0);
                Ok(None)
            }
            Self::Test(step) => {
                let Some(ecat::DeviceResponse::Pdi(recv_bytes)) = received else {
                    return Ok(None);
                };

                if *step < 10000 {
                    *step += 1;
                }

                use ethercrab::EtherCrabWireRead;
                let recv = RecvObj::unpack_from_slice(recv_bytes).unwrap();
                let status = recv.status;

                if *step > 1500 && recv.status & RecvObj::ERR_MASK == RecvObj::ERR_MASK {
                    let mbx_count = subdev.mailbox_counter();
                    let mut read_err = SdoRead::new(mbx_count, 0x603F, 0);

                    let configured_addr = subdev.configured_address();

                    read_err.start(
                        maindevice,
                        retry_count,
                        timeout_duration,
                        tx_entries,
                        sock,
                        ring,
                        &subdev.config.mailbox.write.unwrap(),
                        &subdev.config.mailbox.read.unwrap(),
                        configured_addr,
                        identifier,
                        idx,
                        &write_entry,
                        &timeout_entry,
                    )?;

                    *self = Self::Error(read_err);
                    return Ok(None);
                } else if *step > 3500 && status != 0x1637 {
                    //*step = 0;
                    return Ok(Some(ControlFlow::Restart));
                }

                let step = *step;
                let (ctrl, mut velocity) = if step < 500 {
                    (0x0080, 0)
                } else if step < 1500 {
                    (0x0006, 0)
                } else if step < 2500 {
                    (0x0007, 0)
                } else if step < 3500 {
                    (0x000F, 0)
                } else {
                    (0x000F, -25000)
                };

                if idx > 0 {
                    velocity *= -1;
                }

                use ethercrab::EtherCrabWireWrite;
                let write_obj = WriteObj::new(ctrl, velocity, 9);
                write_obj.pack_to_slice(output_buf).unwrap();
                Ok(None)
            }
            Self::Error(err) => {
                let Some(response) = received else {
                    return Ok(None);
                };

                use ecat::DeviceResponse;
                match response {
                    DeviceResponse::Pdi(bytes) => {
                        use ethercrab::EtherCrabWireRead;
                        let recv = RecvObj::unpack_from_slice(bytes).unwrap();
                        println!("err state: {recv:?}");

                        use ethercrab::EtherCrabWireWrite;
                        let write_obj = WriteObj::new(1 << 7 | 1 << 8, 0, 0);
                        println!("\nsending: {write_obj:?}");
                        write_obj.pack_to_slice(output_buf).unwrap();
                        if err.finished() {
                            *self = Self::Test(0);
                            return Ok(Some(ControlFlow::Restart));
                        }
                    }
                    DeviceResponse::Pdu(received, header) => {
                        if let Some(_err_code) = err.update(
                            received,
                            header,
                            maindevice,
                            retry_count,
                            timeout_duration,
                            tx_entries,
                            sock,
                            ring,
                            &subdev.config.mailbox.write.unwrap(),
                            &subdev.config.mailbox.read.unwrap(),
                            subdev.configured_address(),
                            identifier,
                            idx,
                            &write_entry,
                            &timeout_entry,
                        )? {

                            /*
                            if matches!(err_code, 0 | 0x730F | 0xA000) {
                                //TODO: if in 0xA000, request to move back to the state the motor is in

                                println!("insignificant err, retry rx/tx");
                                let mut buf = [0; 20];
                                use ethercrab::EtherCrabWireWrite;
                                let write_obj = WriteObj::new(0x0080, 0, 9);
                                write_obj.pack_to_slice(&mut buf[12..]).unwrap();

                                println!("send: {buf:?}");

                                // lmao no way thats all it is
                                let (frame, handle) =
                                    unsafe { maindevice.prep_rx_tx(0, &buf).unwrap().unwrap() };
                                ecat::setup::setup_write(
                                    frame,
                                    handle,
                                    retry_count,
                                    timeout_duration,
                                    tx_entries,
                                    sock,
                                    ring,
                                    Some(idx),
                                    None,
                                )?;

                                *self = Self::Test(0);
                                return Ok(Some(ControlFlow::Send));
                            }
                            panic!("error with code 0x{:02x}", err_code);
                            */
                        }
                    }
                }
                Ok(None)
            }
        }
    }
}

#[derive(Debug)]
pub struct CVP {
    pub position: i32,
    pub velocity: i32,
    pub torque: i16,
}
