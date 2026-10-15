// Hooks for the runtime's `memory.rs` (see ../hooks.rs).
&[
    // The resource heap grows whenever the streaming engine asks how much
    // memory is left; on low-memory devices the host lets it grow only for
    // allocations (and only calls GetHeapSize, to know the heap's size).
    ("grow_heap", "rage::sysMemGrowBuddyAllocator::GrowHeap()"),
    ("heap_size", "rage::sysMemGrowBuddyAllocator::GetHeapSize() const"),
    ("memory_available", "rage::sysMemGrowBuddyAllocator::GetMemoryAvailable()"),
    ("largest_block", "rage::sysMemGrowBuddyAllocator::GetLargestAvailableBlock()"),
    // The engine's resource cache budgets from the adapter's video memory
    // (the browser build's adapter says 2 GiB, whatever the device has):
    // the host caps the descriptor, and the engine's own accounting, its
    // settings menu with it, follows the GPU's real budget.
    ("update_total_available_memory", "rage::grcResourceCache::UpdateTotalAvailableMemory()"),
    // The engine's allocator, for what the host hands the engine (the
    // online stand-ins, the cloud's files).
    ("builtin_malloc", "emscripten_builtin_malloc"),
    ("builtin_free", "emscripten_builtin_free"),
]
