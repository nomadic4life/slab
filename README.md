# Incognitus Slab (Zero-Copy)

This project implements the Incognitus slab allocator for Solana accounts with zero-copy access.
It follows the full Layer 1 design: header + stack-node, insert/delete, resize flow, and
fragmented stack handling.

**Why this design**
- Uses a 24-byte header with raw byte offsets for minimal load cost.
- Stores free addresses in an in-account stack for O(1) reuse.
- Uses a stack-node at the end of the account to manage growth and fragmentation.
- All structures are `#[repr(C, align(8))]` so the memory layout is stable.

**Alignment and offsets**
- 8-byte alignment keeps access predictable in BPF and makes bytemuck mapping safe.
- `bytemuck` is used for safe zero-copy casting of account bytes into Rust structs.
- Offsets are stored as `u32`, which limits the slab max size but keeps the header compact.

**Slab model**
- One slab per node type (separate memory pool).
- Fixed-size slots per slab (`SLOT_SIZE`).
- Each slot is addressed by a raw byte offset.

**Free stack as a linked list of offsets**
- The header stores the offset of the first free node.
- A freed node writes the next free offset into its first 8 bytes.
- Allocation pops offsets from this list and jumps straight to the node.

**Core formula**
Offsets are raw byte positions. For fixed-size nodes, valid offsets are aligned and
computed as:
```
Offset = HEADER_SIZE + (index * SLOT_SIZE)
```
The allocator stores and returns these offsets directly, so jumps are O(1) with no array
loading. This is manual memory management over a raw byte buffer, which is exactly what
zero-copy requires on Solana.

**Layer 1 coverage**
- Header (24 bytes) + flags
- Stack-node variants (Head / Tail / Linked)
- Insert/Delete with pop/seq paths
- Resize interrupt flag and resize flow
- Min-swap + cycle pointer (basic)
- Tests for insert/delete and boundary merge
