use crate::txbuf::TxBuf;
use ethercrab::{MainDevice, PduHeader, error::Error, received_frame::ReceivedPdu};
use io_uring::IoUring;
use std::collections::BTreeMap;

use heapless::Deque;

pub struct Op<const N: usize, U> {
    subdevices: Deque<U, N>,
}

impl<const N: usize, U: crate::user::UserDevice> Op<N, U> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_new<S>(
        subdevs: Deque<(U, S), N>,
        maindevice: &mut MainDevice,
        tx_entries: &mut BTreeMap<u64, TxBuf>,
        ring: &mut IoUring,
        mut user_cb: impl FnMut(
            &mut MainDevice,
            &mut U,
            Option<DeviceResponse<'_, '_>>,
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
        write_entry: impl Fn(u64) -> u64,
        timeout_entry: impl Fn(u64) -> u64,
    ) -> Result<Self, Error> {
        let mut subdevices = Deque::new();
        for (id, (mut subdev, _)) in subdevs.into_iter().enumerate() {
            let buf_range = subdev.subdevice().config.io.output.bytes.clone();
            let user_output_buf = &mut output_buf[buf_range];

            user_cb(
                maindevice,
                &mut subdev,
                None,
                tx_entries,
                ring,
                id as _,
                None,
                user_output_buf,
            )
            .map_err(|_| Error::Internal)?;
            let _ = subdevices.push_back(subdev);
        }

        let (frame, handle) = unsafe { maindevice.prep_rx_tx(0, output_buf) }?.unwrap();

        crate::setup::setup_write(
            frame,
            handle,
            retry_count,
            timeout,
            tx_entries,
            sock,
            ring,
            Some(0),
            None,
            write_entry,
            timeout_entry,
        )?;

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
            &mut U,
            Option<crate::op::DeviceResponse<'_, '_>>,
            &mut BTreeMap<u64, TxBuf>,
            &mut IoUring,
            u16,
            Option<u8>,
            &mut [u8],
        ) -> std::io::Result<Option<crate::user::ControlFlow>>,
        input_end: usize,
        transmission_buf: &mut [u8],
        retry_count: usize,
        timeout: &io_uring::types::Timespec,
        sock: &ethercrab::std::RawSocketDesc,
        write_entry: impl Fn(u64) -> u64,
        timeout_entry: impl Fn(u64) -> u64,
    ) -> Result<Option<crate::user::ControlFlow>, Error> {
        let idx = idx.unwrap() as usize;

        if header.command_code == 12 {
            let received_bytes = &received[..];
            (transmission_buf[..received.len()]).copy_from_slice(received_bytes);

            let (input_buf, output_buf) = transmission_buf.split_at_mut(input_end);

            let mut ctrl_flow = None;

            for (id, subdev) in self.subdevices.iter_mut().enumerate() {
                let output_buf_range = subdev.subdevice().config.io.output.bytes.clone();
                let output_buf_range =
                    output_buf_range.start - input_end..output_buf_range.end - input_end;

                let Some(user_output_buf) = output_buf.get_mut(output_buf_range) else {
                    continue;
                };

                let input_buf_range = subdev.subdevice().config.io.input.bytes.clone();
                let Some(user_input_buf) = input_buf.get(input_buf_range) else {
                    continue;
                };

                if let (None, Some(f)) = (
                    ctrl_flow,
                    user_cb(
                        maindevice,
                        subdev,
                        Some(DeviceResponse::Pdi(user_input_buf)),
                        tx_entries,
                        ring,
                        id as _,
                        None,
                        user_output_buf,
                    )
                    .map_err(|_| Error::Internal)?,
                ) {
                    ctrl_flow = Some(f);
                }
            }

            let (frame, handle) = unsafe { maindevice.prep_rx_tx(0, transmission_buf) }?.unwrap();

            crate::setup::setup_write(
                frame,
                handle,
                retry_count,
                timeout,
                tx_entries,
                sock,
                ring,
                Some(0),
                None,
                write_entry,
                timeout_entry,
            )?;

            Ok(ctrl_flow)
        } else {
            let dev = self.subdevices.get_mut(idx).unwrap();
            let output_buf_range = dev.subdevice().config.io.output.bytes.clone();

            let Some(user_output_buf) = transmission_buf.get_mut(output_buf_range) else {
                return Ok(None);
            };

            user_cb(
                maindevice,
                dev,
                Some(DeviceResponse::Pdu(received, header)),
                tx_entries,
                ring,
                idx as _,
                identifier,
                user_output_buf,
            )
            .map_err(|_| Error::Internal)
        }
    }

    pub fn subdev_mut(&mut self, idx: usize) -> Option<&mut U> {
        self.subdevices.get_mut(idx)
    }
}

pub enum DeviceResponse<'a, 'b> {
    Pdu(ReceivedPdu<'a>, PduHeader),
    Pdi(&'b [u8]),
}
