use core::marker::PhantomData;
use core::mem::{align_of, size_of};

use bytemuck::{Pod, Zeroable};

use crate::error::SlabError;

pub const NONE_U32: u32 = u32::MAX;
pub const NONE_U64: u64 = u64::MAX;

// 8-byte alignment keeps offset math simple and predictable for zero-copy.
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct SlabHeader {
    // Raw byte offset where the next sequential node will be written.
    pub bump_offset: u32,
    // Raw byte offset of the first free node in the in-account linked list.
    pub free_stack_head: u32,
}

// Fixed-size header used for the free stack. Stored inside the first 8 bytes of a slot.
// The "pointer" is a raw byte offset (u64) into the account data, not a CPU address.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct FreeNode {
    pub next_free: u64,
}

pub const HEADER_SIZE: usize = size_of::<SlabHeader>();

// Generic slab where each slot is SLOT_SIZE bytes.
// Each node type should have its own slab (separate pool).
pub struct SlabMut<'a, const SLOT_SIZE: usize> {
    data: &'a mut [u8],
    header_ptr: *mut SlabHeader,
    _marker: PhantomData<&'a mut SlabHeader>,
}

impl<'a, const SLOT_SIZE: usize> SlabMut<'a, SLOT_SIZE> {
    pub fn init(data: &'a mut [u8]) -> Result<Self, SlabError> {
        let mut slab = Self::from_account_data(data)?;
        let first = slab.first_node_offset() as u32;
        let header = slab.header_mut();
        *header = SlabHeader {
            bump_offset: first,
            free_stack_head: NONE_U32,
        };
        Ok(slab)
    }

    pub fn from_account_data(data: &'a mut [u8]) -> Result<Self, SlabError> {
        if data.len() < HEADER_SIZE {
            return Err(SlabError::AccountTooSmall);
        }

        let (header_bytes, _rest) = data.split_at_mut(HEADER_SIZE);
        if (header_bytes.as_ptr() as usize) % align_of::<SlabHeader>() != 0 {
            return Err(SlabError::Misaligned);
        }
        let header_ptr = header_bytes.as_mut_ptr() as *mut SlabHeader;

        Ok(Self {
            data,
            header_ptr,
            _marker: PhantomData,
        })
    }

    pub fn header(&self) -> &SlabHeader {
        unsafe { &*self.header_ptr }
    }

    pub fn header_mut(&mut self) -> &mut SlabHeader {
        unsafe { &mut *self.header_ptr }
    }

    fn first_node_offset(&self) -> usize {
        // Align the first slot start to 8 bytes for BPF-friendly access.
        let align = 8usize;
        (HEADER_SIZE + (align - 1)) & !(align - 1)
    }

    fn validate_offset(&self, offset: u32) -> Result<usize, SlabError> {
        let offset = offset as usize;
        let first = self.first_node_offset();
        if offset < first {
            return Err(SlabError::OutOfBounds);
        }
        if offset % 8 != 0 {
            return Err(SlabError::Misaligned);
        }
        let end = offset + SLOT_SIZE;
        if end > self.data.len() {
            return Err(SlabError::OutOfBounds);
        }
        Ok(offset)
    }

    pub fn node_bytes_mut(&mut self, offset: u32) -> Result<&mut [u8], SlabError> {
        // Direct zero-copy access to the slot bytes by raw offset.
        let offset = self.validate_offset(offset)?;
        Ok(&mut self.data[offset..offset + SLOT_SIZE])
    }

    fn free_node_mut(&mut self, offset: u32) -> Result<&mut FreeNode, SlabError> {
        let offset = self.validate_offset(offset)?;
        let bytes = &mut self.data[offset..offset + size_of::<FreeNode>()];
        if (bytes.as_ptr() as usize) % align_of::<FreeNode>() != 0 {
            return Err(SlabError::Misaligned);
        }
        Ok(bytemuck::from_bytes_mut::<FreeNode>(bytes))
    }

    // Pop from the free stack; if empty, bump-allocate a fresh node.
    pub fn pop_free_node(&mut self) -> Result<u32, SlabError> {
        let head = self.header().free_stack_head;
        if head != NONE_U32 {
            let node = self.free_node_mut(head)?;
            // Linked-list of offsets stored inside the freed nodes.
            self.header_mut().free_stack_head = node.next_free as u32;
            return Ok(head);
        }

        let bump = self.header().bump_offset as usize;
        let end = bump + SLOT_SIZE;
        if end > self.data.len() {
            return Err(SlabError::OutOfSpace);
        }
        // Bump allocation returns the next raw byte offset.
        self.header_mut().bump_offset = end as u32;
        Ok(bump as u32)
    }

    // Push a node back onto the free stack.
    pub fn push_free_node(&mut self, offset: u32) -> Result<(), SlabError> {
        let head = self.header().free_stack_head;
        let node = self.free_node_mut(offset)?;
        // Store the next free offset in the first 4 bytes of the freed node.
        node.next_free = head as u64;
        self.header_mut().free_stack_head = offset;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mvp_stack_lifo() {
        let mut data = [0u8; 256];
        let mut slab = SlabMut::<32>::init(&mut data).unwrap();

        let a = slab.pop_free_node().unwrap();
        let b = slab.pop_free_node().unwrap();
        let first = slab.first_node_offset() as u32;
        assert_eq!(a, first);
        assert_eq!(b, (first + 32));

        slab.push_free_node(a).unwrap();
        let c = slab.pop_free_node().unwrap();
        assert_eq!(c, a);
    }
}
