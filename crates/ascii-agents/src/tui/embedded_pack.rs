//! Embeds the bundled default sprite pack into the binary at compile time.

use anyhow::Result;
use ascii_agents_core::sprite::format::{load_pack_from_strings, Pack};

pub fn load_default_pack() -> Result<Pack> {
    let pack_toml = include_str!("../../../../assets/sprites/default/pack.toml");
    let idle = include_str!("../../../../assets/sprites/default/idle.sprite");
    let typing_0 = include_str!("../../../../assets/sprites/default/typing_0.sprite");
    let typing_1 = include_str!("../../../../assets/sprites/default/typing_1.sprite");
    let typing_2 = include_str!("../../../../assets/sprites/default/typing_2.sprite");
    let waiting = include_str!("../../../../assets/sprites/default/waiting.sprite");

    load_pack_from_strings(
        pack_toml,
        &[
            ("idle.sprite", idle),
            ("typing_0.sprite", typing_0),
            ("typing_1.sprite", typing_1),
            ("typing_2.sprite", typing_2),
            ("waiting.sprite", waiting),
        ],
    )
}
