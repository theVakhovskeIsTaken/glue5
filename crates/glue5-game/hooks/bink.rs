// Hooks for the runtime's `bink.rs` (see ../hooks.rs).
&[
    // SetMovieSync is a stub that fails (no Bink SDK in the wasm); the host
    // replaces it with an FFmpeg-backed player that sets up fake BINK/FB
    // structures and game textures. BeginDraw is wrapped so the host decodes
    // a frame into the textures before the original binds them to the shader.
    // Play/Stop set the playing state directly (the original queues to the
    // async thread, which the host bypasses). The rest are call-only entries.
    ("set_movie_sync", "rage::bwMovie::SetMovieSync(rage::bwMovie::bwMovieParams&)"),
    ("begin_draw", "rage::bwMovie::BeginDraw(bool)"),
    ("bink_play", "rage::bwMovie::Play()"),
    ("bink_stop", "rage::bwMovie::Stop()"),
    ("factory_create", "rage::grcTextureFactoryDX11::Create(unsigned int, unsigned int, unsigned int, void*, unsigned int, rage::grcTextureFactory::TextureCreateParams*)"),
    ("lock_rect", "rage::grcTextureDX11::LockRect(int, int, rage::grcTextureLock&, unsigned int) const"),
    ("unlock_rect", "rage::grcTextureDX11::UnlockRect(rage::grcTextureLock const&) const"),
    ("lock_context", "rage::grcDevice::LockContext()"),
    ("unlock_context", "rage::grcDevice::UnlockContext()"),
    ("update_gpu_copy", "rage::grcTextureDX11::UpdateGPUCopy_WithThreadSyncing(unsigned int, unsigned int, void*) const"),
    ("sprite_draw_switch", "CSprite2d::DrawSwitch(bool, bool, float, rage::Vector2 const&, rage::Vector2 const&, rage::Vector2 const&, rage::Vector2 const&, rage::Vector2 const&, rage::Vector2 const&, rage::Vector2 const&, rage::Vector2 const&, rage::Color32 const&)"),
    ("op_new", "operator new(unsigned long)"),
    ("op_new_align", "operator new(unsigned long, unsigned long)"),
]
