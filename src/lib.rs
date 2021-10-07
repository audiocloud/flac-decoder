use std::collections::VecDeque;
use std::io::{Cursor, ErrorKind};

use claxon::frame::FrameReader;
use claxon::input::ReadBytes;
use claxon::metadata::{MetadataBlock, MetadataBlockReader, StreamInfo};
use js_sys::{Float32Array, WebAssembly};
use log::{debug, error, Level};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::utils::set_panic_hook;

mod utils;

#[wasm_bindgen]
pub fn init(debug_log_level: bool) {
    set_panic_hook();
    console_log::init_with_level(if debug_log_level { Level::Debug } else { Level::Info }).expect("init");
}

#[wasm_bindgen]
pub struct Decoder {
    input: Option<Vec<u8>>,
    output: VecDeque<(f32, f32)>,
    left: Vec<f32>,
    right: Vec<f32>,
    stream_info: StreamInfo,
}

#[wasm_bindgen]
impl Decoder {
    pub fn new(buffer: Box<[u8]>) -> Result<Decoder, JsValue> {
        const FLAC_HEADER: u32 = 0x66_4c_61_43;

        debug!("Trying to create FLAC Decoder from {} bytes", buffer.len());

        let mut cursor = Cursor::new(buffer);
        let header = cursor.read_be_u32().map_err(|e| e.to_string())?;
        if header != FLAC_HEADER {
            return Err(format!("Wrong FLAC Header, got: {} expected: {}", header, FLAC_HEADER).into());
        }

        let stream_info = {
            let mut maybe_stream_info = None;
            let metadata_reader = MetadataBlockReader::new(&mut cursor);
            for item in metadata_reader {
                let item = item.map_err(|e| e.to_string())?;
                match item {
                    MetadataBlock::StreamInfo(si) => {
                        maybe_stream_info = Some(si);
                    }
                    _ => {}
                }
            }

            maybe_stream_info.ok_or_else(|| "Missing stream info")?
        };

        let position = cursor.position() as usize;
        let remaining = &cursor.into_inner()[position..];

        let input = if remaining.len() > 0 {
            Some(remaining.into_iter().cloned().collect())
        } else {
            None
        };

        let left = Vec::with_capacity(16 * 1024);
        let right = Vec::with_capacity(16 * 1024);

        Ok(Self { input, output: Default::default(), left, right, stream_info })
    }

    pub fn bit_depth(&self) -> u32 {
        self.stream_info.bits_per_sample
    }

    pub fn sample_rate(&self) -> u32 {
        self.stream_info.sample_rate
    }

    pub fn push(&mut self, data: Box<[u8]>) -> Result<usize, JsValue> {
        debug!("Pushing {} bytes", data.len());
        let mut input = self.input.take().unwrap_or_default();
        input.extend(data.iter());

        let mut total = 0;
        let mut pos = 0;
        let left_shift = 32 - self.bit_depth();

        loop {
            let mut reader = FrameReader::new(Cursor::new(&input[pos..]));
            match reader.read_next_or_eof(Vec::new()) {
                Ok(Some(block)) => {
                    for (l, r) in block.stereo_samples() {
                        let l = ((l << left_shift) as u32).wrapping_add(0x80000000);
                        let r = ((r << left_shift) as u32).wrapping_add(0x80000000);
                        let l = (l as f32) / 2147483648.0 - 1.0;
                        let r = (r as f32) / 2147483648.0 - 1.0;

                        self.output.push_back((l, r));
                    }

                    total += block.duration() as usize;
                    pos += reader.into_inner().position() as usize;
                }
                Ok(None) => {
                    break;
                }
                Err(err) => {
                    match &err {
                        claxon::Error::IoError(err) if err.kind() == ErrorKind::UnexpectedEof => {
                            // this is ok, just break
                            break;
                        }
                        _ => {}
                    }
                    error!("Error while decoding: {:?}", &err);
                    return Err(err.to_string().into());
                }
            }
        }

        self.input = match (pos == 0, pos == input.len()) {
            (_, true) => None,
            (true, _) => Some(input),
            _ => Some(input.as_slice()[pos..].into_iter().cloned().collect())
        };

        Ok(total)
    }

    pub fn pull(&mut self, size: usize) -> usize {
        let mut read_pos = 0;
        for (l, r) in self.output.iter() {
            self.left[read_pos] = *l;
            self.right[read_pos] = *r;

            read_pos += 1;
            if read_pos >= size {
                break;
            }
        }

        read_pos
    }

    pub fn get_left(&self) -> Float32Array {
        let buffer = wasm_bindgen::memory().dyn_into::<WebAssembly::Memory>().unwrap().buffer();
        js_sys::Float32Array::new_with_byte_offset_and_length(
            &buffer,
            self.left.as_ptr() as u32,
            (self.left.capacity() * 4) as u32,
        )
    }

    pub fn get_right(&self) -> Float32Array {
        let buffer = wasm_bindgen::memory().dyn_into::<WebAssembly::Memory>().unwrap().buffer();
        js_sys::Float32Array::new_with_byte_offset_and_length(
            &buffer,
            self.right.as_ptr() as u32,
            (self.right.capacity() * 4) as u32,
        )
    }
}