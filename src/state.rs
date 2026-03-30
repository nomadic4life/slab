use core::marker::PhantomData;
use core::mem::{align_of, size_of};

use bytemuck::{Pod, Zeroable};

use crate::error::SlabError;

pub const FULL_FLAG: u32 = u32::MAX;

// Flags (byte layout: [resize][fragmented][validation][growing])
pub const FLAG_GROWING: u32 = 0x0000_00FF;
pub const FLAG_VALIDATION: u32 = 0x0000_FF00;
pub const FLAG_FRAGMENTED: u32 = 0x00FF_0000;
pub const FLAG_RESIZE_INTERRUPT: u32 = 0xFF00_0000;

pub const STACK_NODE_HEAD: u32 = 0x0000_0000;
pub const STACK_NODE_TAIL: u32 = 0xFFFF_FFFF;

// Slab header (24 bytes) as defined in the spec.
// All pointers are raw byte offsets into the account data.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct SlabHeader {
    pub discriminator: u32,
    pub root_node_pointer: u32,
    pub stack_pointer: u32,
    pub allocator_size: u32,
    pub num_node_elements: u32,
    pub flags: u32,
}

// StackNode is always 16 bytes. Interpretation depends on the type flag.
// Head node (c = 0): a = next_seq_index, b = cycle pointer, d = stack pointer
// Tail node (c = 0xFFFF_FFFF): b = next stack-node address
// Linked node (c = tail_stack_entry_pointer): b = next stack-node address
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct StackNode {
    pub a: u32,
    pub b: u32,
    pub c: u32,
    pub d: u32,
}

pub const HEADER_SIZE: usize = size_of::<SlabHeader>();
pub const STACK_NODE_SIZE: usize = size_of::<StackNode>();

// Generic slab where each slot is SLOT_SIZE bytes.
// Each node type should have its own slab (separate pool).
pub struct SlabMut<'a, const SLOT_SIZE: usize> {
    data: &'a mut [u8],
    header_ptr: *mut SlabHeader,
    _marker: PhantomData<&'a mut SlabHeader>,
}

impl<'a, const SLOT_SIZE: usize> SlabMut<'a, SLOT_SIZE> {
    pub fn init(data: &'a mut [u8], discriminator: u32) -> Result<Self, SlabError> {
        let mut slab = Self::from_account_data(data)?;
        let first = slab.first_node_offset() as u32;
        let head_addr = slab.head_addr()? as u32;

        let alloc_size = slab.data.len() as u32;
        let header = slab.header_mut();
        *header = SlabHeader {
            discriminator,
            root_node_pointer: 0,
            stack_pointer: head_addr,
            allocator_size: alloc_size,
            num_node_elements: 0,
            flags: FLAG_GROWING,
        };

        // Initialize head stack node at end of account.
        slab.write_head_node(first, head_addr);

        Ok(slab)
    }

    pub fn from_account_data(data: &'a mut [u8]) -> Result<Self, SlabError> {
        if data.len() < HEADER_SIZE + STACK_NODE_SIZE {
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

    // Align first node start to SLOT_SIZE.
    fn first_node_offset(&self) -> usize {
        let align = SLOT_SIZE.max(8);
        (HEADER_SIZE + (align - 1)) & !(align - 1)
    }

    fn head_addr(&self) -> Result<usize, SlabError> {
        let len = self.data.len();
        if len < STACK_NODE_SIZE {
            return Err(SlabError::AccountTooSmall);
        }
        Ok(len - STACK_NODE_SIZE)
    }

    fn stack_node_mut(&mut self, addr: usize) -> Result<&mut StackNode, SlabError> {
        let end = addr + STACK_NODE_SIZE;
        if end > self.data.len() {
            return Err(SlabError::OutOfBounds);
        }
        let bytes = &mut self.data[addr..end];
        if (bytes.as_ptr() as usize) % align_of::<StackNode>() != 0 {
            return Err(SlabError::Misaligned);
        }
        Ok(bytemuck::from_bytes_mut::<StackNode>(bytes))
    }

    fn write_head_node(&mut self, next_seq_index: u32, head_addr: u32) {
        let addr = head_addr as usize;
        let node = self.stack_node_mut(addr).expect("head node addr");
        // Head node layout:
        // a = next_seq_index, b = cycle pointer, c = HEAD flag, d = stack pointer
        node.a = next_seq_index;
        node.b = head_addr; // stack_entry_cycle_pointer starts at head_addr
        node.c = STACK_NODE_HEAD;
        node.d = head_addr; // stack_pointer / single_stack_node_flag
    }

    fn head_node_mut(&mut self) -> Result<&mut StackNode, SlabError> {
        let head_addr = self.head_addr()?;
        self.stack_node_mut(head_addr)
    }

    fn validate_offset(&self, offset: u32) -> Result<usize, SlabError> {
        let offset = offset as usize;
        let first = self.first_node_offset();
        if offset < first {
            return Err(SlabError::OutOfBounds);
        }
        if offset % SLOT_SIZE != 0 {
            return Err(SlabError::Misaligned);
        }
        let end = offset + SLOT_SIZE;
        if end > self.data.len() - STACK_NODE_SIZE {
            return Err(SlabError::OutOfBounds);
        }
        Ok(offset)
    }

    fn read_u32(&self, addr: u32) -> Result<u32, SlabError> {
        let addr = addr as usize;
        let end = addr + 4;
        if end > self.data.len() {
            return Err(SlabError::OutOfBounds);
        }
        let bytes: [u8; 4] = self.data[addr..end].try_into().unwrap();
        Ok(u32::from_le_bytes(bytes))
    }

    fn write_u32(&mut self, addr: u32, value: u32) -> Result<(), SlabError> {
        let addr = addr as usize;
        let end = addr + 4;
        if end > self.data.len() {
            return Err(SlabError::OutOfBounds);
        }
        self.data[addr..end].copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    // Direct zero-copy access to slot bytes by raw offset.
    pub fn node_bytes_mut(&mut self, offset: u32) -> Result<&mut [u8], SlabError> {
        let offset = self.validate_offset(offset)?;
        Ok(&mut self.data[offset..offset + SLOT_SIZE])
    }

    pub fn insert(&mut self, node_bytes: &[u8]) -> Result<u32, SlabError> {
        if node_bytes.len() != SLOT_SIZE {
            return Err(SlabError::Misaligned);
        }
        if (self.header().flags & FLAG_RESIZE_INTERRUPT) != 0 {
            return Err(SlabError::ResizeInterrupt);
        }

        let head_addr = self.head_addr()? as u32;
        let sp = self.header().stack_pointer;

        let dest = if sp < head_addr {
            // Pop from stack (recycled addresses).
            let entry = self.read_u32(sp)?;
            if entry == FULL_FLAG {
                self.remove_tail_stack_node(sp)?;
                let new_sp = self.header().stack_pointer;
                let dest = self.read_u32(new_sp)?;
                self.header_mut().stack_pointer = new_sp + 4;
                dest
            } else {
                self.header_mut().stack_pointer = sp + 4;
                entry
            }
        } else {
            // Sequential insert from the head node.
            let stack_top = self.header().stack_pointer;
            let head = self.head_node_mut()?;
            let next_seq = head.a;
            if (next_seq as usize) + SLOT_SIZE > stack_top as usize {
                return Err(SlabError::OutOfSpace);
            }
            head.a = next_seq + SLOT_SIZE as u32;
            self.validate_memory()?;
            next_seq
        };

        let dest_usize = self.validate_offset(dest)?;
        self.data[dest_usize..dest_usize + SLOT_SIZE].copy_from_slice(node_bytes);
        self.header_mut().num_node_elements = self.header().num_node_elements + 1;
        Ok(dest)
    }

    pub fn delete(&mut self, node_addr: u32) -> Result<(), SlabError> {
        if (self.header().flags & FLAG_RESIZE_INTERRUPT) != 0 {
            return Err(SlabError::ResizeInterrupt);
        }
        let _ = self.validate_offset(node_addr)?;

        let sp = self.header().stack_pointer;
        let new_sp = sp.checked_sub(4).ok_or(SlabError::OutOfBounds)?;

        if self.read_u32(new_sp)? == FULL_FLAG {
            self.merge_stack_node(new_sp)?;
        }

        self.write_u32(new_sp, node_addr)?;
        self.header_mut().stack_pointer = new_sp;

        if (self.header().flags & FLAG_FRAGMENTED) == 0 {
            self.maybe_swap_min(node_addr, new_sp)?;
            self.advance_cycle_pointer()?;
        }

        self.header_mut().num_node_elements = self.header().num_node_elements.saturating_sub(1);
        self.validate_memory()?;
        Ok(())
    }

    fn maybe_swap_min(&mut self, node_addr: u32, new_sp: u32) -> Result<(), SlabError> {
        let head = self.head_node_mut()?;
        let cycle = head.b;
        if cycle < new_sp {
            return Ok(());
        }
        let entry = self.read_u32(cycle)?;
        if node_addr < entry {
            self.write_u32(cycle, node_addr)?;
            self.write_u32(new_sp, entry)?;
        }
        Ok(())
    }

    fn advance_cycle_pointer(&mut self) -> Result<(), SlabError> {
        let sp = self.header().stack_pointer;
        let head_addr = self.head_addr()? as u32;
        let head = self.head_node_mut()?;
        let mut cycle = head.b;
        cycle = cycle.saturating_sub(4);
        if cycle < sp {
            cycle = head_addr;
        }
        head.b = cycle;
        Ok(())
    }

    fn validate_memory(&mut self) -> Result<(), SlabError> {
        // Threshold is WIP in the spec; conservative trigger when free space < 2 slots.
        let head = self.head_node_mut()?;
        let next_seq = head.a as usize;
        let sp = self.header().stack_pointer as usize;
        if sp <= next_seq + (SLOT_SIZE * 2) {
            self.header_mut().flags |= FLAG_RESIZE_INTERRUPT;
        }
        Ok(())
    }

    // Called by a separate resize instruction after account growth.
    pub fn resize(&mut self, new_size: usize) -> Result<(), SlabError> {
        if new_size > self.data.len() {
            return Err(SlabError::OutOfBounds);
        }
        let old_head_addr = self.head_addr()? as u32;
        let new_head_addr = (new_size - STACK_NODE_SIZE) as u32;

        // Write a boundary sentinel just before the old head node.
        if old_head_addr < 4 {
            return Err(SlabError::OutOfBounds);
        }
        self.write_u32(old_head_addr - 4, FULL_FLAG)?;

        // Convert old head node into a tail or linked node, depending on current state.
        {
            let is_fragmented = (self.header().flags & FLAG_FRAGMENTED) != 0;
            let old = self.stack_node_mut(old_head_addr as usize)?;
            old.a = FULL_FLAG;
            old.b = new_head_addr;
            if !is_fragmented {
                // First growth: old head becomes tail node.
                old.c = STACK_NODE_TAIL;
            } else {
                // Subsequent growth: old head becomes linked node.
                // c holds tail_stack_entry_pointer (last stack entry in this section).
                old.c = old_head_addr - 4;
            }
            old.d = FULL_FLAG;
        }

        // Write new head at new end.
        let first = self.first_node_offset() as u32;
        self.write_head_node(first, new_head_addr);

        self.header_mut().allocator_size = new_size as u32;
        self.header_mut().flags |= FLAG_FRAGMENTED;
        self.header_mut().flags &= !FLAG_RESIZE_INTERRUPT;
        Ok(())
    }

    fn remove_tail_stack_node(&mut self, sp: u32) -> Result<(), SlabError> {
        // Boundary hit during insert pop. The stack node sits right after the sentinel.
        let node_addr = sp + 4;
        let head_addr = self.head_addr()? as u32;
        let node = self.stack_node_mut(node_addr as usize)?;
        let next = node.b;
        let is_tail = node.c == STACK_NODE_TAIL;

        let new_sp = next.saturating_sub(4);
        self.header_mut().stack_pointer = new_sp;
        if next == head_addr && is_tail {
            self.header_mut().flags &= !FLAG_FRAGMENTED;
        }
        Ok(())
    }

    fn merge_stack_node(&mut self, boundary: u32) -> Result<(), SlabError> {
        // Boundary hit during delete. Merge by switching to the next stack-node section.
        let node_addr = boundary + 4;
        let head_addr = self.head_addr()? as u32;
        let node = self.stack_node_mut(node_addr as usize)?;
        let next = node.b;
        let is_tail = node.c == STACK_NODE_TAIL;
        let new_sp = next.saturating_sub(4);
        self.header_mut().stack_pointer = new_sp;
        if next == head_addr && is_tail {
            self.header_mut().flags &= !FLAG_FRAGMENTED;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_delete_basic() {
        let mut data = [0u8; 1024];
        let mut slab = SlabMut::<32>::init(&mut data, 0x5452_4945).unwrap();

        let node = [1u8; 32];
        let a = slab.insert(&node).unwrap();
        let b = slab.insert(&node).unwrap();
        assert!(a < b);

        slab.delete(a).unwrap();
        let _c = slab.insert(&node).unwrap();
    }

    #[test]
    fn boundary_pop_merges_tail() {
        let mut data = [0u8; 1024];
        let mut slab = SlabMut::<32>::init(&mut data, 0x5452_4945).unwrap();
        let head_addr = slab.head_addr().unwrap() as u32;
        let tail_addr = head_addr - STACK_NODE_SIZE as u32 - 16;

        // Sentinel before tail node.
        slab.write_u32(tail_addr - 4, FULL_FLAG).unwrap();
        slab.header_mut().stack_pointer = tail_addr - 4;

        // Tail node points to head node.
        let tail = slab.stack_node_mut(tail_addr as usize).unwrap();
        tail.a = FULL_FLAG;
        tail.b = head_addr;
        tail.c = STACK_NODE_TAIL;
        tail.d = FULL_FLAG;

        // Provide a valid stack entry for the next section (before head).
        let first = slab.first_node_offset() as u32;
        slab.write_u32(head_addr - 4, first).unwrap();

        let node = [1u8; 32];
        let _ = slab.insert(&node).unwrap();
        assert_eq!(slab.header().stack_pointer, head_addr);
    }

    #[test]
    fn boundary_pop_linked_keeps_fragmented() {
        let mut data = [0u8; 1024];
        let mut slab = SlabMut::<32>::init(&mut data, 0x5452_4945).unwrap();
        let head_addr = slab.head_addr().unwrap() as u32;

        // Mark fragmented and create a linked node after the sentinel.
        slab.header_mut().flags |= FLAG_FRAGMENTED;
        let linked_addr = head_addr - STACK_NODE_SIZE as u32 - 16;
        slab.write_u32(linked_addr - 4, FULL_FLAG).unwrap();
        slab.header_mut().stack_pointer = linked_addr - 4;

        let linked = slab.stack_node_mut(linked_addr as usize).unwrap();
        linked.a = FULL_FLAG;
        linked.b = head_addr; // next points to head
        linked.c = linked_addr - 4; // tail_stack_entry_pointer (non-FULL_FLAG => linked)
        linked.d = FULL_FLAG;

        let first = slab.first_node_offset() as u32;
        slab.write_u32(head_addr - 4, first).unwrap();

        let node = [1u8; 32];
        let _ = slab.insert(&node).unwrap();
        assert!((slab.header().flags & FLAG_FRAGMENTED) != 0);
    }
}
