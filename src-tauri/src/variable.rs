use std::collections::BTreeMap;

use crate::mod_config::ModEntry;



/// LethalDevMode - megumin
/// Imperium - giosuel
/// OdinSerializer - Lordfirespeed
/// LethalNetworkAPI - xilophor
/// 56+
/// CruiserJumpPractice - aoirint

pub fn get_practice_mod_list() -> Vec<ModEntry> {
    vec![
        ModEntry {
            dev: "megumin".to_string(),
            name: "LethalDevMode".to_string(),
            enabled: true,
            low_cap: None,
            high_cap: None,
            version_config: BTreeMap::new(),
        },
        ModEntry {
            dev: "giosuel".to_string(),
            name: "Imperium".to_string(),
            enabled: true,
            low_cap: None,
            high_cap: None,
            version_config: BTreeMap::new(),
        },
        ModEntry {
            dev: "Lordfirespeed".to_string(),
            name: "OdinSerializer".to_string(),
            enabled: true,
            low_cap: None,
            high_cap: None,
            version_config: BTreeMap::new(),
        },
        ModEntry {
            dev: "xilophor".to_string(),
            name: "LethalNetworkAPI".to_string(),
            enabled: true,
            low_cap: None,
            high_cap: None,
            version_config: BTreeMap::new(),
        },
        ModEntry {
            dev: "aoirint".to_string(),
            name: "CruiserJumpPractice".to_string(),
            enabled: true,
            low_cap: None,
            high_cap: None,
            version_config: BTreeMap::new(),
        },
    ]
}