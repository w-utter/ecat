use ecat::{InitState, PdoConfig, PdoMapping, PdoObject, SdoWrite, TxBuf, TxIndex};
use ethercrab::{PduHeader, SubDevice, error::Error, received_frame::ReceivedPdu};

use ecat::io::{TIMEOUT_CLEAR_MASK, TIMEOUT_MASK, WRITE_MASK};

use io_uring::types::Timespec;

const MAX_PDU_DATA: usize = ethercrab::PduStorage::element_size(1100);
const MAX_FRAMES: usize = 64;

static PDU_STORAGE: ethercrab::PduStorage<MAX_FRAMES, MAX_PDU_DATA> = ethercrab::PduStorage::new();

use std::collections::BTreeMap;

use ethercrab::MainDevice;
use ethercrab::std::RawSocketDesc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
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

    let mut state = InitState::<16, UserState>::new();
    state.start(
        &maindevice,
        retries,
        &timeout,
        &mut tx_bufs,
        &sock,
        &mut ring,
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
                        |_| &config,
                        |maindev, subdev, state, received, entries, ring, index, identifier| {
                            let _ = state.update(
                                received, maindev, retries, &timeout, entries, &sock, ring, subdev,
                                index, identifier,
                            );
                            Ok(())
                        },
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

#[derive(Default)]
enum UserState {
    #[default]
    Idle,
    //Init(UserControlInit),
    Test(u32),
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

#[derive(Copy, Clone, ethercrab_wire::EtherCrabWireWrite)]
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
    fn update(
        &mut self,
        received: Option<(ReceivedPdu<'_>, PduHeader)>,
        maindevice: &MainDevice,
        retry_count: usize,
        timeout_duration: &Timespec,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        sock: &RawSocketDesc,
        ring: &mut io_uring::IoUring,
        _subdev: &mut SubDevice,
        idx: u16,
        _identifier: Option<u8>,
    ) -> Result<(), Error> {
        match self {
            Self::Idle => {
                println!("attempting to start rx/tx");

                let mut buf = [0; 20];
                use ethercrab::EtherCrabWireWrite;
                let write_obj = WriteObj::new(0x0080, 0, 9);
                write_obj.pack_to_slice(&mut buf[12..]).unwrap();

                println!("send: {buf:?}");

                // lmao no way thats all it is
                let (frame, handle) = unsafe { maindevice.prep_rx_tx(0, &buf).unwrap().unwrap() };
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

                // TODO: bring this back once testing tx/rx loop
                /*
                let mut ctrl = UserControlInit::new(subdev, 9);
                ctrl.start(maindevice, retry_count, timeout_duration, tx_entries, sock, ring, subdev, idx);

                *self = Self::Init(ctrl);
                */
            }
            Self::Test(step) => {
                if *step < 10000 {
                    *step += 1;
                }
                let (received, _header) = received.unwrap();

                use ethercrab::EtherCrabWireRead;
                let recv = RecvObj::unpack_from_slice(&*received).unwrap();

                println!("step {step} state: {:?}", recv);

                let step = *step;
                let (ctrl, velocity) = if step < 1000 {
                    (0x0080, 0)
                } else if step < 1500 {
                    (0x0006, 0)
                } else if step < 2000 {
                    (0x0007, 0)
                } else if step < 2500 {
                    (0x000F, 0)
                } else {
                    (0x000F, -25000)
                };

                let mut buf = [0; 20];
                use ethercrab::EtherCrabWireWrite;
                let write_obj = WriteObj::new(ctrl, velocity, 9);
                write_obj.pack_to_slice(&mut buf[12..]).unwrap();

                let (frame, handle) = unsafe { maindevice.prep_rx_tx(0, &buf).unwrap().unwrap() };
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
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct CVP {
    pub position: i32,
    pub velocity: i32,
    pub torque: i16,
}
