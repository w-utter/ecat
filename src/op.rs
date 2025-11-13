use crate::txbuf::TxBuf;
use ethercrab::{MainDevice, PduHeader, SubDevice, error::Error, received_frame::ReceivedPdu};
use io_uring::IoUring;
use std::collections::BTreeMap;

use heapless::Deque;

pub struct Op<const N: usize, T> {
    subdevices: Deque<(SubDevice, T), N>,
}

impl<const N: usize, T: Default> Op<N, T> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_new<S>(
        subdevs: Deque<(SubDevice, S), N>,
        maindevice: &mut MainDevice,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        ring: &mut IoUring,
        mut user_cb: impl FnMut(
            &mut MainDevice,
            &mut ethercrab::SubDevice,
            &mut T,
            Option<(ReceivedPdu<'_>, PduHeader)>,
            &mut BTreeMap<u64, TxBuf>,
            &mut IoUring,
            u16,
            Option<u8>,
            &mut [u8],
        ) -> std::io::Result<Option<crate::user::ControlFlow>>,
        output_buf: &mut [u8],
        retry_count: usize,
        timeout: &io_uring::types::Timespec,
        sock: &ethercrab::std::RawSocketDesc,
    ) -> Result<Self, Error> {
        let mut subdevices = Deque::new();
        for (id, (mut subdev, _)) in subdevs.into_iter().enumerate() {
            let mut state = T::default();

            println!("dev state: {:?}", subdev.config.io);

            let buf_range = subdev.config.io.output.bytes.clone();
            let user_output_buf = &mut output_buf[buf_range];

            if let Some(flow) = user_cb(
                maindevice,
                &mut subdev,
                &mut state,
                None,
                tx_entries,
                ring,
                id as _,
                None,
                user_output_buf,
            )
            .map_err(|_| Error::Internal)?
            {
                use crate::user::ControlFlow;
                match flow {
                    ControlFlow::Send => {
                        let (frame, handle) =
                            unsafe { maindevice.prep_rx_tx(0, output_buf) }?.unwrap();

                        crate::setup::setup_write(
                            frame,
                            handle,
                            retry_count,
                            timeout,
                            tx_entries,
                            sock,
                            ring,
                            Some(id as _),
                            None,
                        )?;
                    }
                }
            }
            let _ = subdevices.push_back((subdev, state));
        }

        Ok(Self { subdevices })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn update(
        &mut self,
        received: ReceivedPdu<'_>,
        header: PduHeader,
        maindevice: &mut MainDevice,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        ring: &mut IoUring,
        identifier: Option<u8>,
        idx: Option<u16>,
        mut user_cb: impl FnMut(
            &mut MainDevice,
            &mut ethercrab::SubDevice,
            &mut T,
            Option<(ReceivedPdu<'_>, PduHeader)>,
            &mut BTreeMap<u64, TxBuf>,
            &mut IoUring,
            u16,
            Option<u8>,
            &mut [u8],
        ) -> std::io::Result<Option<crate::user::ControlFlow>>,
        output_buf: &mut [u8],
    ) -> Result<Option<crate::user::ControlFlow>, Error> {
        let idx = idx.unwrap() as usize;
        let (dev, state) = self.subdevices.get_mut(idx).unwrap();

        let buf_range = dev.config.io.output.bytes.clone();
        let user_output_buf = &mut output_buf[buf_range];

        user_cb(
            maindevice,
            dev,
            state,
            Some((received, header)),
            tx_entries,
            ring,
            idx as _,
            identifier,
            user_output_buf,
        )
        .map_err(|_| Error::Internal)
    }

    pub fn subdev_mut(&mut self, idx: usize) -> Option<&mut (SubDevice, T)> {
        self.subdevices.get_mut(idx)
    }
}
