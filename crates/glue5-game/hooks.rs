// Game functions the host can take over.
//
// Each entry pairs a field name with the function's name in the wasm name
// section. The build script finds the function, derives its signature from the
// module and routes every call to it through `Hooks::<field>`. An unset hook
// falls back to the original, and a hook can call the original through
// `orig::<field>`. Hooks receive the same `*mut Env` as imports. An entry
// the host leaves unset gives it a way to call that game function.
//
// This file is included by the build script.
//
// The hooks are grouped by the runtime module that implements them
// (`crates/glue5-runtime/src/<group>.rs`, whose `hooks()` sets them).

pub const HOOKS: &[&[(&str, &str)]] = &[
    include!("hooks/input.rs"),
    include!("hooks/exit.rs"),
    include!("hooks/gamelog.rs"),
    include!("hooks/memory.rs"),
    include!("hooks/files.rs"),
    include!("hooks/skin.rs"),
    include!("hooks/shader_settings.rs"),
    include!("hooks/shadow_reveal.rs"),
    include!("hooks/device_reset.rs"),
    include!("hooks/readback.rs"),
    include!("hooks/audio_update.rs"),
    include!("hooks/granular.rs"),
    include!("hooks/script_assets.rs"),
    include!("hooks/gamer_tags.rs"),
    include!("hooks/scenario.rs"),
    include!("hooks/tour.rs"),
    include!("hooks/online.rs"),
    include!("hooks/vnet.rs"),
    include!("hooks/cloud.rs"),
    include!("hooks/jobs.rs"),
    include!("hooks/texts.rs"),
    include!("hooks/stats.rs"),
    include!("hooks/upscale.rs"),
    include!("hooks/boot_screen.rs"),
];
