//! Double-buffered shared-memory pixels for the popup surface.

use std::fs::File;
use std::os::fd::AsFd;

use memmap2::MmapMut;
use wayland_client::QueueHandle;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;

use super::wayland::State;

/// Identifies a buffer: the pool generation and the slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BufferId {
    pub generation: u32,
    pub slot: usize,
}

struct Slot {
    buffer: Option<WlBuffer>,
    width: u32,
    height: u32,
    /// Attached and not yet released by the compositor.
    busy: bool,
}

/// Two buffers in one pool, each large enough for the biggest image so far.
pub(crate) struct Buffers {
    pool: Option<(WlShmPool, MmapMut)>,
    generation: u32,
    slot_bytes: usize,
    slots: [Slot; 2],
}

impl Buffers {
    pub fn new() -> Buffers {
        let slot = || Slot {
            buffer: None,
            width: 0,
            height: 0,
            busy: false,
        };
        Buffers {
            pool: None,
            generation: 0,
            slot_bytes: 0,
            slots: [slot(), slot()],
        }
    }

    /// Copies `pixels` (premultiplied ARGB8888 rows of `width`) into a free
    /// buffer of `width` by `height` and returns it; `pixels` may be smaller,
    /// the rest is transparent. `None` when both buffers are still in use or
    /// memory could not be allocated.
    pub fn fill(
        &mut self,
        shm: &WlShm,
        qh: &QueueHandle<State>,
        width: u32,
        height: u32,
        pixels: &[u32],
        src_width: u32,
    ) -> Option<WlBuffer> {
        let bytes = width as usize * height as usize * 4;
        if bytes > self.slot_bytes {
            self.grow(shm, qh, bytes)?;
        }
        let slot = self.slots.iter().position(|s| !s.busy)?;
        let (pool, map) = self.pool.as_mut()?;
        let s = &mut self.slots[slot];
        if s.buffer.is_none() || (s.width, s.height) != (width, height) {
            if let Some(old) = s.buffer.take() {
                old.destroy();
            }
            let offset = (slot * self.slot_bytes) as i32;
            let id = BufferId {
                generation: self.generation,
                slot,
            };
            s.buffer = Some(pool.create_buffer(
                offset,
                width as i32,
                height as i32,
                width as i32 * 4,
                wl_shm::Format::Argb8888,
                qh,
                id,
            ));
            (s.width, s.height) = (width, height);
        }
        let dst = &mut map[slot * self.slot_bytes..slot * self.slot_bytes + bytes];
        let src_rows = pixels.len() / src_width.max(1) as usize;
        for (y, row) in dst.chunks_exact_mut(width as usize * 4).enumerate() {
            row.fill(0);
            if y < src_rows {
                let src = &pixels[y * src_width as usize..(y + 1) * src_width as usize];
                for (d, p) in row.as_chunks_mut::<4>().0.iter_mut().zip(src) {
                    *d = p.to_ne_bytes();
                }
            }
        }
        s.busy = true;
        s.buffer.clone()
    }

    /// The compositor finished reading a buffer.
    pub fn released(&mut self, id: BufferId) {
        if id.generation == self.generation {
            self.slots[id.slot].busy = false;
        }
    }

    fn grow(&mut self, shm: &WlShm, qh: &QueueHandle<State>, bytes: usize) -> Option<()> {
        let slot_bytes = bytes.next_power_of_two().max(64 * 1024);
        let fd =
            rustix::fs::memfd_create("ghostcaret-popup", rustix::fs::MemfdFlags::CLOEXEC).ok()?;
        let file = File::from(fd);
        file.set_len((2 * slot_bytes) as u64).ok()?;
        // SAFETY: the file is a fresh memfd only this process and the
        // compositor share; the compositor reads a buffer only between attach
        // and release, and `fill` writes only buffers that are not attached.
        let map = unsafe { MmapMut::map_mut(&file) }.ok()?;
        let pool = shm.create_pool(file.as_fd(), (2 * slot_bytes) as i32, qh, ());
        if let Some((old, _)) = self.pool.take() {
            old.destroy();
        }
        for s in &mut self.slots {
            if let Some(b) = s.buffer.take() {
                b.destroy();
            }
            s.busy = false;
        }
        self.generation += 1;
        self.slot_bytes = slot_bytes;
        self.pool = Some((pool, map));
        Some(())
    }
}
