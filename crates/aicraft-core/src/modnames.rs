//! Modrinth 的 slug → MC 模组百科的条目号。
//!
//! 百科的地址是 `https://www.mcmod.cn/class/{编号}.html`，编号只能查表，
//! 没法从 slug 推出来。这里只收两边都确实存在的项目：查错了会把人送到一个
//! 不相干的模组页，比不显示这个按钮更糟。
//!
//! 将来补条目时往 `MCMOD` 里加行就行，别处不用动。

const MCMOD: &[(&str, &str)] = &[
    ("actually-additions", "550"),
    ("adventofascension", "1269"),
    ("ae2", "260"),
    ("aether", "94"),
    ("appleskin", "744"),
    ("argonmod", "16737"),
    ("ars-nouveau", "3468"),
    ("better-advancements", "1530"),
    ("better-combat", "7110"),
    ("betterend", "3163"),
    ("betternether", "1579"),
    ("biomes-o-plenty", "108"),
    ("blood-magic", "219"),
    ("botania", "332"),
    ("buildcraft", "4"),
    ("carpet", "2361"),
    ("chicken-chunks", "167"),
    ("cobblemon", "9300"),
    ("construction-wand", "3545"),
    ("crafting-tweaks", "1501"),
    ("crafttweaker", "669"),
    ("create", "2021"),
    ("curios", "2029"),
    ("cyclic", "834"),
    ("draconic-evolution", "423"),
    ("enchantment-descriptions", "1945"),
    ("ender-storage", "76"),
    ("enderio", "181"),
    ("entity-culling", "3058"),
    ("entityculling", "3629"),
    ("environmental-tech", "583"),
    ("epic-fight", "3037"),
    ("explorers-compass", "4395"),
    ("extreme-reactors", "814"),
    ("farmers-delight", "2820"),
    ("ferrite-core", "3888"),
    ("forestry", "5"),
    ("galacticraft-legacy", "194"),
    ("ice-and-fire-dragons", "770"),
    ("immediatelyfast", "7948"),
    ("immersiveengineering", "463"),
    ("indium", "3413"),
    ("industrial-craft", "2"),
    ("industrial-foregoing", "979"),
    ("integrated-dynamics", "845"),
    ("inventoryhudplus", "3395"),
    ("iris", "3697"),
    ("iron-chests", "20"),
    ("irons-spells-n-spellbooks", "10175"),
    ("item-scroller", "1529"),
    ("itemphysic", "932"),
    ("jade", "3482"),
    ("jei", "459"),
    ("journeymap", "198"),
    ("krypton", "3399"),
    ("litematica", "2261"),
    ("lithium", "2292"),
    ("logistics-pipes", "145"),
    ("lootr", "2924"),
    ("mekanism", "187"),
    ("memoryleakfix", "6593"),
    ("minecraft-transit-railway", "2157"),
    ("minihud", "2311"),
    ("modmenu", "1675"),
    ("modular-routers", "892"),
    ("mouse-tweaks", "1162"),
    ("mutant-monsters", "10081"),
    ("mystical-agriculture", "929"),
    ("natures-compass", "754"),
    ("nuclearcraft", "388"),
    ("pams-harvestcraft", "218"),
    ("patchouli", "1388"),
    ("physicsmod", "3460"),
    ("pipez", "3532"),
    ("refined-storage", "691"),
    ("rubidium", "5608"),
    ("simple-storage-network", "1714"),
    ("smoothboot-fabric", "3422"),
    ("sodium", "2785"),
    ("sodium-extra", "3701"),
    ("sophisticated-backpacks", "3739"),
    ("sophisticated-core", "6324"),
    ("storagedrawers", "408"),
    ("thermal-dynamics", "480"),
    ("thermal-expansion", "3824"),
    ("thermal-foundation", "425"),
    ("tinkers-construct", "101"),
    ("travelersbackpack", "1732"),
    ("waystones", "1339"),
    ("xaeros-minimap", "1701"),
    ("xaeros-world-map", "1483"),
    ("yungs-better-caves", "1981"),
    ("yungs-better-desert-temples", "6613"),
    ("yungs-better-dungeons", "4429"),
    ("yungs-better-jungle-temples", "12060"),
    ("yungs-better-mineshafts", "2788"),
    ("yungs-better-nether-fortresses", "9384"),
    ("yungs-better-ocean-monuments", "7904"),
    ("yungs-better-strongholds", "3787"),
    ("yungs-better-witch-huts", "6618"),
];

/// 查这个 Modrinth 项目在 MC 模组百科上的条目号。
///
/// 只有模组才在百科上有条目（资源包、光影包不收录），所以调用方要先确认类别。
pub fn mcmod_id(slug: &str) -> Option<&'static str> {
    let slug = slug.trim();
    if slug.is_empty() {
        return None;
    }
    MCMOD
        .iter()
        .find(|(key, _)| *key == slug)
        .map(|(_, id)| *id)
}

/// 表里认得多少个百科条目。
pub fn mcmod_count() -> usize {
    MCMOD.len()
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn a_known_slug_finds_its_wiki_page() {
        assert_eq!(mcmod_id("create"), Some("2021"));
        assert_eq!(mcmod_id("ae2"), Some("260"));
        assert_eq!(mcmod_id("  sodium  "), Some("2785"));
    }

    #[test]
    fn an_unknown_or_empty_slug_has_no_wiki_page() {
        assert_eq!(mcmod_id("some-mod-that-is-not-listed"), None);
        assert_eq!(mcmod_id(""), None);
        assert_eq!(mcmod_id("   "), None);
    }

    #[test]
    fn the_wiki_table_is_not_a_token_gesture() {
        assert!(mcmod_count() >= 60, "百科表只有 {} 条", mcmod_count());
    }

    #[test]
    fn no_wiki_entry_is_listed_twice() {
        for (index, (slug, _)) in MCMOD.iter().enumerate() {
            let repeated = MCMOD[..index].iter().any(|(other, _)| other == slug);
            assert!(!repeated, "slug 重复：{}", slug);
        }
    }

    #[test]
    fn every_wiki_entry_points_at_a_class_page() {
        for (slug, id) in MCMOD {
            assert!(!slug.is_empty() && slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'), "slug 不像 slug：{}", slug);
            assert!(!id.is_empty() && id.chars().all(|c| c.is_ascii_digit()), "条目号不是数字：{} → {}", slug, id);
        }
    }
}
