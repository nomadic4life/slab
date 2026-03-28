# Incognitus Slab (Zero-Copy)

This project implements a simple slab designed for Solana accounts with zero-copy access.
It is a functional MVP meant to validate the core allocator behavior before full spec compliance.

**Why this design**
- Uses a small header with 32-bit offsets to keep read costs low.
- Reuses free nodes with a LIFO stack to minimize compute units on insert/delete.
- The bump allocator avoids linear scans when the free stack is empty.
- All structures are `#[repr(C, align(8))]` so the memory layout is stable.

**Alignment and offsets**
- 8-byte alignment keeps access predictable in BPF and makes bytemuck mapping safe.
- `bytemuck` is used for safe zero-copy casting of account bytes into Rust structs.
- Offsets are stored as `u32`, which limits the slab max size but keeps the header compact.

**MVP scope**
- Single account, fixed-size nodes.
- LIFO free stack + bump allocation.
- No resize, no stack-node variants, no min-swap optimization yet.

**The "byte addressing" blocker, resolved**
In zero-copy on Solana you do not load a full array into RAM. You jump directly to a byte
offset inside the account data. The "pointer" is a `u32` offset, not a CPU address.

**Free stack as a linked list of offsets**
- The header stores the offset of the first free node.
- A freed node writes the next free offset into its first 4 bytes.
- Allocation pops offsets from this list and jumps straight to the node.

**Core formula**
Offsets are raw byte positions. For fixed-size nodes, valid offsets are aligned and
computed as:
```
Offset = HEADER_SIZE + (index * NODE_SIZE)
```
The allocator stores and returns these offsets directly, so jumps are O(1) with no array
loading. This is manual memory management over a raw byte buffer, which is exactly what
zero-copy requires on Solana.
