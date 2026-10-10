use std::{ptr, sync::{atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering}, Mutex, RwLock}};
use fnv::{FnvHashMap, FnvHashSet};
use sqlparser::ast;
use once_cell::sync::Lazy;
use crate::{
    core::{utils::{get_masterdb_path, get_meta_path}, Hachimi, game::Region},
    il2cpp::{ext::{StringExt, Il2CppStringExt}, hook::{LibNative_Runtime::Sqlite3::{Connection, Query}, umamusume::SceneManager}, types::{Il2CppObject, Il2CppString}}
};
use chrono::{Utc, Datelike};
use rust_i18n::locale;

pub static RETRIEVED_RAW_KEY: Lazy<Mutex<Vec<u8>>> = Lazy::new(|| Mutex::new(Vec::new()));
pub static AUTO_UNLOCK_NEXT_DB: AtomicBool = AtomicBool::new(false);
pub static META_DATA: Lazy<RwLock<MetaData>> = Lazy::new(|| RwLock::new(MetaData::default()));

// public API
#[derive(Default)]
pub struct CharacterData {
    pub chara_ids: FnvHashSet<i32>,
    pub chara_names: FnvHashMap<i32, String>
}

impl CharacterData {
    pub fn load_from_db() -> Self {
        let mut chara_ids = FnvHashSet::default();
        let mut chara_names = FnvHashMap::default();

        let db_path = get_masterdb_path();
        let conn = Connection::new();

        if Connection::Open(conn, db_path.to_il2cpp_string(), ptr::null_mut(), ptr::null_mut(), 0) {
            let sql = "SELECT C.id, T.text FROM chara_data AS C JOIN text_data AS T ON C.id = T.\"index\" WHERE T.id = 6";
            let query = Connection::Query(conn, sql.to_il2cpp_string());

            if !query.is_null() {
                while Query::Step(query) {
                    let id = Query::GetInt(query, 0);
                    let name_ptr = Query::GetText(query, 1);

                    if let Some(name) = unsafe { name_ptr.as_ref() }.map(|s| s.as_utf16str().to_string()) {
                        chara_ids.insert(id);
                        chara_names.insert(id, name);
                    }
                }
                Query::Dispose(query);
            }
            Connection::CloseDB(conn);
        }

        CharacterData { chara_ids, chara_names }
    }

    pub fn exists(&self, id: i32) -> bool {
        self.chara_ids.contains(&id)
    }

    pub fn get_name(&self, id: i32) -> String {
        // check text_data_dict.json (category 170)
        if let Some(category_170) = Hachimi::instance().localized_data.load().text_data_dict.get(&170) {
            if let Some(name) = category_170.get(&id) {
                return name.clone();
            }
        }

        // fallback to default Japanese name from mdb
        if let Some(name) = self.chara_names.get(&id) {
            return name.clone();
        }

        // unknown character name
        "???".to_string()
    }
}

// untranslated skill info
#[derive(Default)]
pub struct SkillInfo {
    pub skill_names: FnvHashMap<i32, String>,
    pub skill_descs: FnvHashMap<i32, String>,
}

impl SkillInfo {
    pub fn load_from_db() -> Self {
        let mut skill_names = FnvHashMap::default();
        let mut skill_descs = FnvHashMap::default();

        let db_path = get_masterdb_path();
        let conn = Connection::new();

        if Connection::Open(conn, db_path.to_il2cpp_string(), ptr::null_mut(), ptr::null_mut(), 0) {
            // category 47 = names, 48 = descriptions
            let sql = "SELECT \"index\", text, id FROM text_data WHERE id IN (47, 48)";
            let query = Connection::Query(conn, sql.to_il2cpp_string());

            if !query.is_null() {
                while Query::Step(query) {
                    let index = Query::GetInt(query, 0);
                    let text_ptr = Query::GetText(query, 1);
                    let category = Query::GetInt(query, 2);

                    if let Some(text) = unsafe { text_ptr.as_ref() }.map(|s| s.as_utf16str().to_string()) {
                        match category {
                            47 => skill_names.insert(index, text),
                            48 => skill_descs.insert(index, text),
                            _ => None,
                        };
                    }
                }
                Query::Dispose(query);
            }
            Connection::CloseDB(conn);
        }

        SkillInfo { skill_names, skill_descs }
    }

    pub fn get_name(&self, id: i32) -> String {
        if let Some(name) = self.skill_names.get(&id) {
            return name.clone();
        }

        // unknown skill name
        "???".to_string()
    }

    pub fn get_desc(&self, id: i32) -> String {
        if let Some(desc) = self.skill_descs.get(&id) {
            return desc.clone();
        }

        // unknown skill desc
        "???".to_string()
    }
}

// All of this add column/param stuff could be simplified to two hash maps, but that's overkill.
pub trait SelectQueryState {
    /// Adds a column to the query.
    ///
    /// Implementers are expected to only track the index of columns that they need.
    fn add_column(&mut self, idx: i32, name: &str);

    /// Adds a placeholder parameter to the query (WHERE param = ?).
    ///
    /// Index starts at 1.
    fn add_param(&mut self, idx: i32, name: &str);

    /// Bind an int value to a placeholder.
    ///
    /// Index starts at 1.
    fn bind_int(&mut self, idx: i32, value: i32);

    /// Gets the resulting string on the current row's column.
    fn get_text(&self, query: *mut Il2CppObject, idx: i32) -> Option<*mut Il2CppString>;
}

#[derive(Default)]
struct Column {
    /// Index of the column in the SELECT statement.
    ///
    /// Can be used to query the value later if needed.
    select_idx: Option<i32>,

    /// Index of the placeholder param for this column.
    ///
    /// If this column's value is already binded as a param in the query, we won't need to query it later.
    param_idx: Option<i32>,

    /// The int value binded to this column as a parameter.
    int_value: Option<i32>
}

impl Column {
    fn is_select_idx(&self, idx: i32) -> bool {
        if let Some(i) = self.select_idx {
            idx == i
        }
        else {
            false
        }
    }

    fn is_param_idx(&self, idx: i32) -> bool {
        if let Some(i) = self.param_idx {
            idx == i
        }
        else {
            false
        }
    }

    fn try_bind_int(&mut self, idx: i32, value: i32) {
        if self.is_param_idx(idx) {
            self.int_value = Some(value);
        }
    }

    fn try_get_int(&self, query: *mut Il2CppObject) -> Option<i32> {
        if let Some(idx) = self.select_idx {
            Some(Query::GetInt(query, idx))
        }
        else {
            None
        }
    }

    fn value_or_try_get_int(&self, query: *mut Il2CppObject) -> Option<i32> {
        if let Some(value) = self.int_value {
            Some(value)
        }
        else if let Some(value) = self.try_get_int(query) {
            Some(value)
        }
        else {
            None
        }
    }
}

#[derive(Default)]
pub struct SkillDataDesc {
    pub descs: FnvHashMap<i32, String>
}

struct SkillDataDescRow {
    id: i32,
    precondition_1: String,
    condition_1: String,
    ability_time_1: i32,
    cooldown_time_1: i32,
    precondition_2: String,
    condition_2: String,
    ability_time_2: i32,
    cooldown_time_2: i32,
    slots: [SkillDataDescSlot; 6]
}

#[derive(Clone, Copy, Default)]
struct SkillDataDescSlot {
    ability_type: i32,
    ability_value: i32,
    ability_value_usage: i32,
    additional_activate_type: i32,
    target_type: i32,
    target_value: i32
}

impl SkillDataDesc {
    pub fn load_from_db() -> Self {
        let mut descs = FnvHashMap::default();

        let db_path = get_masterdb_path();
        let conn = Connection::new();

        if Connection::Open(conn, db_path.to_il2cpp_string(), ptr::null_mut(), ptr::null_mut(), 0) {
            let sql = "SELECT id, \
                precondition_1, condition_1, float_ability_time_1, float_cooldown_time_1, \
                ability_type_1_1, ability_value_usage_1_1, additional_activate_type_1_1, float_ability_value_1_1, target_type_1_1, target_value_1_1, \
                ability_type_1_2, ability_value_usage_1_2, additional_activate_type_1_2, float_ability_value_1_2, target_type_1_2, target_value_1_2, \
                ability_type_1_3, ability_value_usage_1_3, additional_activate_type_1_3, float_ability_value_1_3, target_type_1_3, target_value_1_3, \
                precondition_2, condition_2, float_ability_time_2, float_cooldown_time_2, \
                ability_type_2_1, ability_value_usage_2_1, additional_activate_type_2_1, float_ability_value_2_1, target_type_2_1, target_value_2_1, \
                ability_type_2_2, ability_value_usage_2_2, additional_activate_type_2_2, float_ability_value_2_2, target_type_2_2, target_value_2_2, \
                ability_type_2_3, ability_value_usage_2_3, additional_activate_type_2_3, float_ability_value_2_3, target_type_2_3, target_value_2_3 \
                FROM skill_data";
            let query = Connection::Query(conn, sql.to_il2cpp_string());

            if !query.is_null() {
                while Query::Step(query) {
                    let row = Self::get_data_row(query);
                    let desc = Self::format_data_desc(&row);
                    descs.insert(row.id, desc);
                }
                Query::Dispose(query);
            }
            Connection::CloseDB(conn);
        }

        SkillDataDesc { descs }
    }

    pub fn get_desc(&self, id: i32) -> Option<&String> {
        self.descs.get(&id)
    }
    
    fn get_data_slot(query: *mut Il2CppObject, base: i32) -> SkillDataDescSlot {
        SkillDataDescSlot {
            ability_type: Query::GetInt(query, base),
            ability_value_usage: Query::GetInt(query, base + 1),
            additional_activate_type: Query::GetInt(query, base + 2),
            ability_value: Query::GetInt(query, base + 3),
            target_type: Query::GetInt(query, base + 4),
            target_value: Query::GetInt(query, base + 5)
        }
    }

    fn get_data_text(query: *mut Il2CppObject, idx: i32) -> String {
        let text_ptr = Query::GetText(query, idx);
        unsafe { text_ptr.as_ref() }.map(|s| s.as_utf16str().to_string()).unwrap_or_default()
    }

    fn get_data_row(query: *mut Il2CppObject) -> SkillDataDescRow {
        SkillDataDescRow {
            id: Query::GetInt(query, 0),
            precondition_1: Self::get_data_text(query, 1),
            condition_1: Self::get_data_text(query, 2),
            ability_time_1: Query::GetInt(query, 3),
            cooldown_time_1: Query::GetInt(query, 4),
            precondition_2: Self::get_data_text(query, 23),
            condition_2: Self::get_data_text(query, 24),
            ability_time_2: Query::GetInt(query, 25),
            cooldown_time_2: Query::GetInt(query, 26),
            slots: [
                Self::get_data_slot(query, 5), Self::get_data_slot(query, 11), Self::get_data_slot(query, 17),
                Self::get_data_slot(query, 27), Self::get_data_slot(query, 33), Self::get_data_slot(query, 39)
            ]
        }
    }

    fn round_ties_up(value: i32, units: i32) -> i32 {
        let rem = value.rem_euclid(units);
        let base = value - rem;
        if rem * 2 >= units { base + units } else { base }
    }

    fn format_data_number(value: i32, div: i32, decimals: usize) -> String {
        let units = div / 10i32.pow(decimals as u32);
        let rounded = Self::round_ties_up(value, units);
        let neg = rounded < 0;
        let abs = rounded.unsigned_abs() as u64;
        let div = div as u64;
        let whole = abs / div;
        let frac = (abs % div) / (div / 10u64.pow(decimals as u32));

        let mut out = String::new();
        if neg {
            out.push('-');
        }
        out.push_str(&whole.to_string());
        if frac > 0 {
            out.push('.');
            let frac_str = format!("{:0width$}", frac, width = decimals);
            out.push_str(frac_str.trim_end_matches('0'));
        }
        out
    }

    fn str(key: &str) -> Option<String> {
        let full_key = format!("skill_data_desc.{key}");
        let localized_data = Hachimi::instance().localized_data.load();
        if let Some(text) = localized_data.skill_data_desc_dict.get(full_key.as_str()) {
            return Some(text.to_string());
        }
        let locale = locale();
        crate::_rust_i18n_try_translate(&locale, full_key.as_str()).map(|text| text.to_string())
    }

    fn data_fmt(key: &str, value: &str) -> Option<String> {
        Self::str(key).map(|text| text.replace("%{v}", value))
    }

    fn op_tag(op: &str) -> &str {
        match op {
            "==" => "eq",
            "!=" => "ne",
            "<=" => "le",
            ">=" => "ge",
            "<" => "lt",
            ">" => "gt",
            _ => "op"
        }
    }

    fn format_effect(slot: SkillDataDescSlot) -> Option<String> {
        let (name_key, unit_key, div, decimals) = match slot.ability_type {
            1 => ("speed_stat", "stat", 10000, 2),
            2 => ("stamina_stat", "stat", 10000, 2),
            3 => ("power_stat", "stat", 10000, 2),
            4 => ("guts_stat", "stat", 10000, 2),
            5 => ("wit_stat", "stat", 10000, 2),
            8 => ("field_of_view", "deg", 10000, 2),
            9 => ("current_hp", "percent", 100, 1),
            13 => ("rushed_time", "second", 10000, 2),
            14 => ("delay_start", "second", 10000, 2),
            21 => ("current_speed", "mps", 10000, 2),
            22 => ("current_speed_natural_decel", "mps", 10000, 2),
            27 => ("target_speed", "mps", 10000, 2),
            28 => ("lane_movement_speed", "percent", 100, 1),
            29 => ("rushed_chance", "stat", 10000, 2),
            31 => ("acceleration", "mps2", 10000, 2),
            32 => ("all_stats", "stat", 10000, 2),
            35 => ("target_lane", "stat", 10000, 2),
            37 => ("activate_rare_skill", "stat", 10000, 2),
            42 | 48 | 49 => ("special", "stat", 10000, 2),
            501 => ("event_specific", "stat", 10000, 2),

            6 => return Self::str("effect.fixed.aggressive_strategy"),
            38 => return Self::str("effect.fixed.debuff_immunity"),
            41 => return Self::str("effect.fixed.sympathy_all"),
            502 => return Self::str("effect.fixed.loh_stat"),
            10 => return Self::str(&format!("effect.start_reaction.{}", slot.ability_value)),
            503 | _ => return None,
        };

        let name = Self::str(&format!("effect.name.{name_key}"))?;
        let unit = Self::str(&format!("effect.unit.{unit_key}")).unwrap_or_default();
        let value = Self::format_data_number(slot.ability_value, div, decimals);
        let sign = if slot.ability_value > 0 { " +" } else { " " };
        let mut out = format!("{name}{sign}{value}{unit}");
        if slot.ability_value_usage == 19 {
            out.push_str(&Self::str("effect.usage19_suffix").unwrap_or_default());
        }

        let star = match slot.additional_activate_type {
            1 => Self::str("star.activate.1"),
            2 => Self::str("star.activate.2"),
            3 => Self::str("star.activate.3"),
            _ => None
        }.or_else(|| {
            if slot.ability_value_usage != 1 {
                Self::str(&format!("star.usage.{}", slot.ability_value_usage))
            } else {
                None
            }
        });
        if let Some(star) = star {
            out.push_str(&Self::str("sep.star").unwrap_or_default());
            out.push_str(&star);
        }

        if slot.target_type != 1 {
            let target = match slot.target_type {
                4 => Self::str("target.all_in_fov"),
                7 => Self::data_fmt("target.leading", &(slot.target_value - 1).to_string()),
                9 => if slot.target_value == 18 {
                    Self::str("target.all_ahead")
                } else {
                    Self::data_fmt("target.closest_ahead", &slot.target_value.to_string())
                },
                10 => if slot.target_value == 18 {
                    Self::str("target.all_behind")
                } else {
                    Self::data_fmt("target.closest_behind", &slot.target_value.to_string())
                },
                11 => Self::str("target.team"),
                18 => match Self::str(&format!("target.style.{}", slot.target_value)) {
                    Some(text) => Some(text),
                    None => return None
                },
                19 => Self::data_fmt("target.random_rushed_ahead", &slot.target_value.to_string()),
                20 => Self::data_fmt("target.random_rushed_behind", &slot.target_value.to_string()),
                21 => match Self::str(&format!("target.style_rushed.{}", slot.target_value)) {
                    Some(text) => Some(text),
                    None => return None
                },
                22 => Self::str("target.suzuka"),
                23 => Self::data_fmt("target.random_recovery_users", &slot.target_value.to_string()),
                24 => Self::str("target.unknown"),
                _ => None
            };
            if let Some(target) = target {
                out.push_str(&Self::str("sep.to").unwrap_or_default());
                out.push_str(&target);
            }
        }

        Some(out)
    }

    fn format_data_conditions(condition: &str) -> String {
        let or_sep = Self::str("sep.or").unwrap_or_default();
        let and_sep = Self::str("sep.and").unwrap_or_default();
        let mut out = String::new();
        for (i, group) in condition.split('@').enumerate() {
            if i > 0 {
                out.push_str(&or_sep);
            }
            for (j, atom) in group.split('&').enumerate() {
                if j > 0 {
                    out.push_str(&and_sep);
                }
                out.push_str(&Self::format_data_atom(atom));
            }
        }
        out
    }

    fn format_data_atom(atom: &str) -> String {
        let bytes = atom.as_bytes();
        let mut token_end = 0;
        while token_end < bytes.len() && (bytes[token_end].is_ascii_lowercase() || bytes[token_end] == b'_' || bytes[token_end].is_ascii_digit()) {
            token_end += 1;
        }
        let op_start = token_end;
        let mut op_end = op_start;
        while op_end < bytes.len() && (bytes[op_end] == b'=' || bytes[op_end] == b'!' || bytes[op_end] == b'<' || bytes[op_end] == b'>') {
            op_end += 1;
        }
        let token = &atom[..token_end];
        let op = &atom[op_start..op_end];
        let value = atom[op_end..].parse::<i32>().unwrap_or(0);

        if token == "order_rate" {
            let text = match op {
                ">" => Self::data_fmt("cond.order_rate.gt", &(100 - value).to_string()),
                ">=" => Self::data_fmt("cond.order_rate.ge", &(100 - value).to_string()),
                "<=" => Self::data_fmt("cond.order_rate.le", &value.to_string()),
                "<" => Self::data_fmt("cond.order_rate.lt", &value.to_string()),
                _ => None
            };
            if let Some(text) = text {
                return text;
            }
        }

        if token == "corner" {
            let text = match (op, value) {
                ("==", 0) => Self::str("cond.corner.straight"),
                ("==", _) => Self::data_fmt("cond.corner.corner", &value.to_string()),
                ("!=", 0) => Self::str("cond.corner.any"),
                ("!=", _) => Self::data_fmt("cond.corner.not", &value.to_string()),
                _ => None
            };
            if let Some(text) = text {
                return text;
            }
        }

        if token == "phase" && matches!(op, "==" | "!=" | "<=" | ">=") {
            if let Some(name) = Self::str(&format!("cond.phase.name.{value}")) {
                return match op {
                    "==" => name,
                    "!=" => Self::data_fmt("cond.negate", &name).unwrap_or_default(),
                    "<=" => Self::data_fmt("cond.phase.le", &name).unwrap_or_default(),
                    _ => Self::data_fmt("cond.phase.ge", &name).unwrap_or_default()
                };
            }
        }

        if token == "ground_condition" && matches!(op, "==" | "!=" | "<=" | ">=") {
            if let Some(name) = Self::str(&format!("cond.ground_condition.name.{value}")) {
                if let Some(text) = Self::data_fmt(&format!("cond.ground_condition.{}", Self::op_tag(op)), &name) {
                    return text;
                }
            }
        }

        if token == "track_id" {
            if op == "<=" && value == 10010 {
                if let Some(text) = Self::str("cond.track_id.jra") {
                    return text;
                }
            }
            if op == ">=" && value == 10001 {
                if let Some(text) = Self::str("cond.track_id.any") {
                    return text;
                }
            }
            if op == "==" || op == "!=" {
                if let Some(name) = Self::str(&format!("cond.track_name.{value}")) {
                    let key = if op == "==" { "cond.track_id.at" } else { "cond.track_id.not_at" };
                    if let Some(text) = Self::data_fmt(key, &name) {
                        return text;
                    }
                }
            }
        }

        if token == "same_skill_horse_count" && op == "==" {
            let text = if value == 1 {
                Self::str("cond.same_skill_horse_count.unique")
            } else {
                Self::data_fmt("cond.same_skill_horse_count.count", &value.to_string())
            };
            if let Some(text) = text {
                return text;
            }
        }

        if token == "near_infront_count" && op == "==" {
            let text = if value == 0 {
                Self::str("cond.near_infront_count.none")
            } else {
                Self::data_fmt("cond.near_infront_count.count", &value.to_string())
            };
            if let Some(text) = text {
                return text;
            }
        }

        if let Some(text) = Self::data_condition_enum(token, op, value) {
            return text;
        }

        if op == "!=" {
            if let Some(text) = Self::data_condition_enum(token, "==", value) {
                if let Some(negated) = Self::data_fmt("cond.negate", &text) {
                    return negated;
                }
            }
        }

        if let Some(text) = Self::data_condition_fixed(token, op) {
            return text;
        }

        if token == "distance_diff_top_float" && op == "<=" {
            if let Some(text) = Self::data_fmt("cond.template.distance_diff_top_float.le", &Self::format_data_number(value, 10, 1)) {
                return text;
            }
        }

        if let Some(text) = Self::data_condition_template(token, op, value) {
            return text;
        }

        if token == "furlong" && op == "==" {
            if let Some(text) = Self::data_fmt("cond.furlong", &(value + 1).to_string()) {
                return text;
            }
        }

        if token == "is_used_skill_id" && op == "==" {
            if let Some(text) = Self::str(&format!("cond.used_skill.{value}")) {
                return text;
            }
            if let Some(text) = Self::data_fmt("cond.used_skill.template", &value.to_string()) {
                return text;
            }
        }

        if token == "is_used_skill_id_with_detail_one" && op == "==" {
            if let Some(text) = Self::str(&format!("cond.used_skill_detail_one.{value}")) {
                return text;
            }
            if let Some(text) = Self::data_fmt("cond.used_skill_detail_one.template", &value.to_string()) {
                return text;
            }
        }

        if token == "is_popularity_top_character_activate_advantage_skill" && op == "==" {
            if value == -1 {
                if let Some(text) = Self::str("cond.popularity_top.any") {
                    return text;
                }
            }
            if let Some(text) = Self::data_fmt("cond.popularity_top.count", &value.to_string()) {
                return text;
            }
        }

        format!("{token} {op} {value}")
    }

    fn data_condition_enum(token: &str, op: &str, value: i32) -> Option<String> {
        Self::str(&format!("cond.enum.{token}.{}.{}", Self::op_tag(op), value))
    }

    fn data_condition_fixed(token: &str, op: &str) -> Option<String> {
        Self::str(&format!("cond.fixed.{token}.{}", Self::op_tag(op)))
    }

    fn data_condition_template(token: &str, op: &str, value: i32) -> Option<String> {
        Self::str(&format!("cond.template.{token}.{}", Self::op_tag(op)))
            .map(|text| text.replace("%{v}", &value.to_string()))
    }

    fn format_data_group(condition: &str, precondition: &str, ability_time: i32, cooldown_time: i32, slots: &[SkillDataDescSlot]) -> Option<String> {
        let mut effects: Vec<String> = Vec::new();
        for slot in slots {
            if slot.ability_type == 0 && slot.ability_value == 0 {
                continue;
            }
            if let Some(effect) = Self::format_effect(*slot) {
                effects.push(effect);
            }
        }
        if effects.is_empty() {
            return None;
        }

        let first_type = slots.first().map(|s| s.ability_type).unwrap_or(0);
        let first_value = slots.first().map(|s| s.ability_value).unwrap_or(0);
        let time_suffix = if ability_time > 0 {
            Self::data_fmt("group.duration", &Self::format_data_number(ability_time, 10000, 2))
        } else if ability_time == 0 {
            Self::str("group.immediate")
        } else if first_type == 21 && first_value < 0 {
            Self::str("group.long_negative")
        } else {
            Self::str("group.indefinite")
        }.unwrap_or_default();

        let mut body = effects.join(", ");
        body.push(' ');
        body.push_str(&time_suffix);

        let mut line = format!("<b>{body}</b>");
        if cooldown_time > 0 && cooldown_time < 5000000 {
            line.push_str(&Self::data_fmt("group.cd", &format!("{:.1}", cooldown_time as f64 / 10000.0)).unwrap_or_default());
        }
        line.push_str(&Self::str("group.when").unwrap_or_default());
        line.push_str(&Self::format_data_conditions(if condition.is_empty() { "always==1" } else { condition }));
        if !precondition.is_empty() {
            line.push_str(&Self::str("group.after").unwrap_or_default());
            line.push_str(&Self::format_data_conditions(precondition));
        }
        Some(line)
    }

    fn format_data_desc(row: &SkillDataDescRow) -> String {
        let group1 = Self::format_data_group(&row.condition_1, &row.precondition_1, row.ability_time_1, row.cooldown_time_1, &row.slots[0..3]);
        let group2 = Self::format_data_group(&row.condition_2, &row.precondition_2, row.ability_time_2, row.cooldown_time_2, &row.slots[3..6]);

        match (group1, group2) {
            (Some(g1), Some(g2)) => format!("{g1}\n{g2}"),
            (Some(g1), None) => g1,
            (None, Some(g2)) => g2,
            (None, None) => String::new()
        }
    }
}

// text_data
#[derive(Default)]
pub struct TextDataQuery {
    // SELECT
    text: Column,

    // WHERE
    category: Column,
    index: Column
}

impl TextDataQuery {
    pub fn get_skill_name(index: i32) -> Option<*mut Il2CppString> {
        // Return None if skill name translation is disabled
        if Hachimi::instance().config.load().disable_skill_name_translation {
            return None;
        }

        let localized_data = Hachimi::instance().localized_data.load();
        localized_data.text_data_dict
            .get(&47)
            .and_then(|c| c.get(&index))
            .map(|t| t.to_il2cpp_string())
    }

    pub fn get_factor_name(index: i32) -> Option<*mut Il2CppString> {
        if Hachimi::instance().config.load().disable_factor_name_translation {
            return None;
        }

        let localized_data = Hachimi::instance().localized_data.load();
        localized_data.text_data_dict
            .get(&147)
            .and_then(|c| c.get(&index))
            .map(|t| t.to_il2cpp_string())
    }

    pub fn get_skill_desc(index: i32) -> Option<*mut Il2CppString> {
        if Hachimi::instance().config.load().skill_data_desc {
            let skill_data_desc = Hachimi::instance().skill_data_desc.load();
            if let Some(desc) = skill_data_desc.get_desc(index) {
                return Some(desc.to_il2cpp_string());
            }
        }

        let localized_data = Hachimi::instance().localized_data.load();
        localized_data
            .text_data_dict
            .get(&48)
            .and_then(|c| c.get(&index))
            .map(|t| t.to_il2cpp_string())
    }
}

impl SelectQueryState for TextDataQuery {
    fn add_column(&mut self, idx: i32, name: &str) {
        if name == "text" {
            self.text.select_idx = Some(idx)
        }
    }

    fn add_param(&mut self, idx: i32, name: &str) {
        match name {
            "category" => self.category.param_idx = Some(idx),
            "index" => self.index.param_idx = Some(idx),
            _ => ()
        }
    }

    fn bind_int(&mut self, idx: i32, value: i32) {
        self.category.try_bind_int(idx, value);
        self.index.try_bind_int(idx, value);
    }

    fn get_text(&self, _query: *mut Il2CppObject, idx: i32) -> Option<*mut Il2CppString> {
        if !self.text.is_select_idx(idx) {
            return None;
        }

        if let Some(category) = self.category.int_value {
            if let Some(index) = self.index.int_value {
                // specialized handlers
                match category {
                    47 => return Self::get_skill_name(index),
                    48 => return Self::get_skill_desc(index),
                    147 => return Self::get_factor_name(index),
                    _ => ()
                };

                return Hachimi::instance().localized_data.load()
                    .text_data_dict
                    .get(&category)
                    .map(|c| c.get(&index).map(|s| s.to_il2cpp_string()))
                    .unwrap_or_default()
            }
        }

        None
    }
}

// character_system_text
#[derive(Default)]
pub struct CharacterSystemTextQuery {
    // SELECT
    text: Column,

    // WHERE
    character_id: Column,

    // may appear in both
    voice_id: Column
}

impl SelectQueryState for CharacterSystemTextQuery {
    fn add_column(&mut self, idx: i32, name: &str) {
        match name {
            "text" => self.text.select_idx = Some(idx),
            "voice_id" => self.voice_id.select_idx = Some(idx),
            _ => ()
        }
    }

    fn add_param(&mut self, idx: i32, name: &str) {
        match name {
            "character_id" => self.character_id.param_idx = Some(idx),
            "voice_id" => self.voice_id.param_idx = Some(idx),
            _ => ()
        }
    }

    fn bind_int(&mut self, idx: i32, value: i32) {
        self.character_id.try_bind_int(idx, value);
        self.voice_id.try_bind_int(idx, value);
    }

    fn get_text(&self, query: *mut Il2CppObject, idx: i32) -> Option<*mut Il2CppString> {
        if !self.text.is_select_idx(idx) {
            return None;
        }

        if let Some(character_id) = self.character_id.int_value {
            if let Some(voice_id) = self.voice_id.value_or_try_get_int(query) {
                return Hachimi::instance().localized_data.load()
                    .character_system_text_dict
                    .get(&character_id)
                    .map(|c| c.get(&voice_id).map(|s| s.to_il2cpp_string()))
                    .unwrap_or_default()
            }
        }

        None
    }
}

// race_jikkyo_comment
#[derive(Default)]
pub struct RaceJikkyoCommentQuery {
    // SELECT
    id: Column,
    message: Column
}

impl SelectQueryState for RaceJikkyoCommentQuery {
    fn add_column(&mut self, idx: i32, name: &str) {
        match name {
            "id" => self.id.select_idx = Some(idx),
            "message" => self.message.select_idx = Some(idx),
            _ => ()
        }
    }

    fn add_param(&mut self, _idx: i32, _name: &str) {}

    fn bind_int(&mut self, _idx: i32, _value: i32) {}

    fn get_text(&self, query: *mut Il2CppObject, idx: i32) -> Option<*mut Il2CppString> {
        if !self.message.is_select_idx(idx) {
            return None;
        }

        if let Some(id) = self.id.try_get_int(query) {
            return Hachimi::instance().localized_data.load()
                .race_jikkyo_comment_dict
                .get(&id)
                .map(|s| s.to_il2cpp_string())
        }

        None
    }
}

// race_jikkyo_message
#[derive(Default)]
pub struct RaceJikkyoMessageQuery {
    // SELECT
    id: Column,
    message: Column
}

impl SelectQueryState for RaceJikkyoMessageQuery {
    fn add_column(&mut self, idx: i32, name: &str) {
        match name {
            "id" => self.id.select_idx = Some(idx),
            "message" => self.message.select_idx = Some(idx),
            _ => ()
        }
    }

    fn add_param(&mut self, _idx: i32, _name: &str) {}

    fn bind_int(&mut self, _idx: i32, _value: i32) {}

    fn get_text(&self, query: *mut Il2CppObject, idx: i32) -> Option<*mut Il2CppString> {
        if !self.message.is_select_idx(idx) {
            return None;
        }

        if let Some(id) = self.id.try_get_int(query) {
            return Hachimi::instance().localized_data.load()
                .race_jikkyo_message_dict
                .get(&id)
                .map(|s| s.to_il2cpp_string())
        }

        None
    }
}


// sqlparser extensions
pub trait SelectExt {
    fn get_first_table_name(&self) -> Option<&String>;
}

impl SelectExt for ast::Select {
    fn get_first_table_name(&self) -> Option<&String> {
        if let Some(table_with_joins) = self.from.get(0) {
            if let ast::TableFactor::Table { name: object_name, .. } = &table_with_joins.relation {
                if let Some(ident) = object_name.0.get(0) {
                    return Some(&ident.value);
                }
            }
        }

        None
    }
}

pub trait SelectItemExt {
    fn get_unnamed_expr_ident(&self) -> Option<&String>;
}

impl SelectItemExt for ast::SelectItem {
    fn get_unnamed_expr_ident(&self) -> Option<&String> {
        if let ast::SelectItem::UnnamedExpr(expr) = self {
            return expr.get_ident_value();
        }

        None
    }
}

pub trait ExprExt {
    fn binary_op_iter<'a>(&'a self) -> BinaryOpIter<'a>;
    fn get_ident_value(&self) -> Option<&String>;
    fn is_placeholder_value(&self) -> bool;
}

impl ExprExt for ast::Expr {
    fn binary_op_iter<'a>(&'a self) -> BinaryOpIter<'a> {
        BinaryOpIter { stack: vec![self] }
    }

    fn get_ident_value(&self) -> Option<&String> {
        if let ast::Expr::Identifier(ident) = self {
            return Some(&ident.value);
        }

        None
    }

    fn is_placeholder_value(&self) -> bool {
        if let ast::Expr::Value(value) = self {
            if let ast::Value::Placeholder(_) = value {
                return true;
            }
        }

        false
    }
}

pub struct BinaryOpIter<'a> {
    stack: Vec<&'a ast::Expr>
}

pub struct BinaryOpRef<'a> {
    pub left: &'a Box<ast::Expr>,
    pub op: &'a ast::BinaryOperator,
    pub right: &'a Box<ast::Expr>
}

impl<'a> Iterator for BinaryOpIter<'a> {
    type Item = BinaryOpRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let Some(expr) = self.stack.pop() else {
                return None;
            };

            let ast::Expr::BinaryOp { left, op, right } = expr else {
                continue;
            };

            self.stack.push(right);
            self.stack.push(left); // left will be pop'd first

            return Some(BinaryOpRef { left, op, right })
        }
    }
}

/// The last path component of a bundle name, under either separator a client reports.
#[inline]
pub fn bundle_file_name(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

/// That component with a trailing extension removed (`atlas_common.a` -> `atlas_common`), and
/// unchanged when the name carries no extension at all - which is how this client names bundles.
#[inline]
pub fn bundle_file_stem(name: &str) -> &str {
    let file = bundle_file_name(name);
    match file.rfind('.') {
        None | Some(0) => file,
        Some(idx) => &file[..idx],
    }
}

/// The `a` table's `n` column, indexed under every form a client may report for the bundle it names.
///
/// The old key was `format!("{}.a", ..)` built off `n`, an extension this table does not have:
/// `get_champions_live_max_year` below reads `n` values as `live/image/champions/tex_championslive_year_<i>`
/// and parses what is left after that prefix as an `i32`, which a trailing `.a` would break on every
/// row. That key could only ever be hit by a client reporting the bundle under a name carrying exactly
/// that extension. Indexed under the name the table actually holds - whole, last component, component
/// without extension - it answers every pair the old shape could have answered (an observed
/// `<component>.a` reaches the row through `bundle_file_stem`) and the pairs it could not, which on
/// this install is the ordinary case: a bundle named with no extension at all.
fn index_logical_name(table: &mut FnvHashMap<String, String>, logical_name: &str, hash: &str) {
    for key in [logical_name, bundle_file_name(logical_name), bundle_file_stem(logical_name)] {
        if !key.is_empty() {
            table.insert(key.to_owned(), hash.to_owned());
        }
    }
}

/// What this client's own `meta` table says about the two bundle names standing in front of the
/// asset patch identity guard (C10, `hook/UnityEngine_AssetBundleModule/AssetBundle.rs`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetaIdentity {
    /// This client records, for the bundle being loaded, exactly the identity the patch recorded.
    Same,
    /// The table names both sides and they are **different logical bundles**: this asset came from a
    /// bundle the patch was not authored for, and that is known rather than guessed.
    Different {
        /// The logical name this client has for the identity the patch recorded.
        expected_logical: String,
        /// The logical name of the bundle being loaded.
        observed_logical: String,
    },
    /// The table says nothing decidable: it is not readable here, it does not name the bundle being
    /// loaded, or the two identities name the same logical bundle under different ids - which is what
    /// a package authored against another client's install looks like from here, and is indistinguishable
    /// from a bundle whose contents changed. Absence of evidence, not evidence of a difference.
    Unknown,
}

#[derive(Default)]
pub struct MetaData {
    /// Every form the `a` table's `n` column can be reported under -> the `h` value it records.
    pub name_to_hash: FnvHashMap<String, String>,
    /// The other direction: a recorded `h` value -> the `n` value it belongs to. This is the direction
    /// a client whose bundles are named by their physical id needs, to say which logical bundle the
    /// asset in front of it belongs to.
    pub hash_to_name: FnvHashMap<String, String>,
}

impl MetaData {
    /// One row of the `a` table.
    fn record(&mut self, logical_name: &str, hash: &str) {
        index_logical_name(&mut self.name_to_hash, logical_name, hash);
        if !hash.is_empty() {
            self.hash_to_name.insert(hash.to_owned(), logical_name.to_owned());
        }
    }

    fn hash_for(&self, observed: &str) -> Option<&str> {
        [observed, bundle_file_name(observed), bundle_file_stem(observed)]
            .into_iter()
            .find_map(|key| self.name_to_hash.get(key).map(|hash| hash.as_str()))
    }

    fn logical_for(&self, recorded: &str) -> Option<&str> {
        self.hash_to_name.get(recorded).map(|name| name.as_str())
    }

    /// The logical bundle name this table holds for a value reported either as a recorded id (`h`, how
    /// a Global install names the bundle it loads) or as a bundle name (`n`, in any of the three
    /// forms, how a Japan install reports it). `None` means this client's table does not know it.
    fn logical_label(&self, value: &str) -> Option<&str> {
        self.logical_for(value)
            .or_else(|| self.hash_for(value).and_then(|hash| self.logical_for(hash)))
    }

    /// The C10 comparison, over data only: no game call, no lock, and no allocation unless the answer
    /// is a refusal.
    ///
    /// A different id on its own is *not* a mismatch. The identity a data package records is the id the
    /// authoring install has for that bundle, and a bundle with text baked into it - every UI atlas -
    /// hashes differently between the Japan and Global packages, so a well authored patch routinely
    /// records an id this install does not have. Refusing on that alone is how an identity check turns
    /// into patches silently not applying. Only a pair this client's own table names as two different
    /// bundles is a refusal, and both sides have to be known for that.
    pub fn compare_identity(&self, expected: &str, observed: &str) -> MetaIdentity {
        if expected == observed {
            return MetaIdentity::Same;
        }

        // The table's own record for the bundle being loaded settles it directly when it agrees.
        if self.hash_for(observed) == Some(expected) {
            return MetaIdentity::Same;
        }

        let (Some(expected_label), Some(observed_label)) =
            (self.logical_label(expected), self.logical_label(observed))
        else {
            return MetaIdentity::Unknown;
        };

        // Known under both names but the same bundle under a different id: a bundle whose contents
        // changed and a bundle seen from another install look identical from here, and neither is a
        // reason to refuse.
        if expected_label == observed_label {
            return MetaIdentity::Unknown;
        }

        MetaIdentity::Different {
            expected_logical: expected_label.to_owned(),
            observed_logical: observed_label.to_owned(),
        }
    }

    fn load_from_db() -> Self {
        let mut meta = MetaData::default();

        let db_path_str = get_meta_path();

        let conn = Connection::new();

        if Hachimi::instance().game.region == Region::Japan {
            AUTO_UNLOCK_NEXT_DB.store(true, Ordering::Relaxed);
        }

        if Connection::Open(conn, db_path_str.to_il2cpp_string(), std::ptr::null_mut(), std::ptr::null_mut(), 0) {
            let sql = "SELECT n, h FROM a";
            let query = Connection::Query(conn, sql.to_il2cpp_string());

            if !query.is_null() {
                while Query::Step(query) {
                    let path_ptr = Query::GetText(query, 0);
                    let hash_ptr = Query::GetText(query, 1);

                    if let (Some(path_str), Some(hash_str)) = (
                        unsafe { path_ptr.as_ref() }.map(|s| s.as_utf16str().to_string()),
                        unsafe { hash_ptr.as_ref() }.map(|s| s.as_utf16str().to_string()),
                    ) {
                        meta.record(&path_str, &hash_str);
                    }
                }
                Query::Dispose(query);
            }
            Connection::CloseDB(conn);
        } else {
            error!("Failed to open meta database at: {}", db_path_str);
        }

        meta
    }
}

/// What the next identity lookup is allowed to do about the `meta` table. This enum is the *read*
/// half: it answers what the state is without charging for anything, which is all the asset patch
/// guard needs before it decides whether converting a bundle name is worth a try
/// (`AssetBundle::check_asset_bundle_name`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableRead {
    /// The table is in memory; read it.
    Read,
    /// The armed window still has a try left, or the settled state is due for one of its bounded key
    /// watches. Whether it comes out as a database open, a key bail or a watch is decided - and
    /// charged - by `TableAttempts::authorise`.
    Attempt,
    /// The window is spent and the settled state has no key watch left to take. Every later lookup
    /// answers `MetaIdentity::Unknown` without touching the game, a lock or the log - until a
    /// witnessed change in the game's database key re-arms the window (`TableAttempts::plan`).
    GiveUp,
}

/// What one patched-asset lookup was allowed to do, **and has already paid for**.
///
/// The difference between the two enums is who pays. `TableRead` is a peek; a `TableStep` comes out
/// of `TableAttempts::authorise`, where the permission and the charge leave the same call. C10's cost
/// half broke exactly on that seam: `plan` granted an `Attempt`, `identity_of`'s key bail returned
/// `Unknown` without charging for the lock it had just taken, so the state never settled and
/// `META_TABLE_KEY_BAILS_CAP` was a number production only ever read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableStep {
    /// The table is in memory; read it. No key lock, no open.
    Read,
    /// A database key had been retrieved: this lookup owns one counted database open.
    Open,
    /// This lookup took the key lock and found no key in it: counted, and it spent no open.
    Bail,
    /// The settled state's bounded look for the observable change that re-opens it: one key lock,
    /// charged against `META_TABLE_KEY_WATCH_CAP`, no open and no table. Answer `Unknown` unless the
    /// lock had a key in it, in which case this lookup owns the open the re-arm exists to fund.
    Watch,
    /// The window is spent and no key watch is due. Answer `MetaIdentity::Unknown` without a lock, a
    /// game call or a log line.
    GiveUp,
}

/// The policy behind those answers: a bounded number of real read attempts, a bounded number of
/// lookups that could not attempt one, then a remembered outcome that a witnessed key transition can
/// re-open.
///
/// The old shape of this test was `logical_name_to_hash.is_empty()`, which is not a "loaded" marker:
/// a table that could not be read leaves the map empty, so *every* later lookup re-ran the whole open
/// - the write lock, `get_meta_path()`, `Connection::new`, `Connection::Open`, a managed string
/// allocation and an `error!` line - on a path one asset load walks (AGENTS section 6 keeps all of
/// those off it). Remembering the outcome is what makes the cost one-off instead of per load.
///
/// The counters are private because charging them is not a service this type offers: the only writer
/// either one has is `authorise`, which grants a try and takes its payment in the same call. A cap
/// some caller may choose not to pay for is not a cap, and that is exactly how
/// `META_TABLE_KEY_BAILS_CAP` ended up dead code.
///
/// What those counters bound is an **armed window**, and a window is not terminal. The one fact that
/// changes what the identity table is worth asking - whether the game has handed a database key over
/// yet - is written by `core::hachimi::sqlite3_key_hook`, and on a real client that lands *after* the
/// first patched-asset loads. A machine whose only inputs were its own two counters had no channel
/// left for that fact, and it settled exactly where it hurt: a patch whose recorded name matches the
/// bundle answers at `AssetBundle::bundle_names_match` before any charge, so the whole key-bail budget
/// is spent by the mismatched loads the table exists to arbitrate, and once it is spent the guard was
/// locked out for the rest of the session even after the key arrived. So each machine counts the key
/// generations it has *seen* (`note_key_answer`: it has looked and found no key, then it looked and
/// found one), a window is armed against one of them, and a witnessed transition re-arms it. A settled
/// window spends at most `META_TABLE_KEY_WATCH_CAP` key locks - at doubling intervals - looking for
/// that transition, and `META_TABLE_REARMS_CAP` bounds the re-arming itself, so the whole machine is
/// `(1 + META_TABLE_REARMS_CAP)` windows and nothing loops.
pub struct TableAttempts {
    attempts: AtomicU8,
    lookups_without_a_key: AtomicU8,
    key_watches: AtomicU8,
    settled_lookups: AtomicU32,
    /// The key generation this window is armed against, and the number of transitions this machine has
    /// witnessed since. They differ for exactly as long as a re-arm is owed.
    armed_generation: AtomicU8,
    generations_seen: AtomicU8,
    /// The two halves of the transition, remembered separately: this machine has looked for a key and
    /// found none, and this machine has since seen one. Latched, so a key that stays in the lock is one
    /// transition and not one per load.
    key_seen_absent: AtomicBool,
    key_seen_present: AtomicBool,
    rearms: AtomicU8,
    loaded: AtomicBool,
    announced: AtomicBool,
}

impl TableAttempts {
    pub const fn new() -> Self {
        Self {
            attempts: AtomicU8::new(0),
            lookups_without_a_key: AtomicU8::new(0),
            key_watches: AtomicU8::new(0),
            settled_lookups: AtomicU32::new(0),
            armed_generation: AtomicU8::new(0),
            generations_seen: AtomicU8::new(0),
            key_seen_absent: AtomicBool::new(false),
            key_seen_present: AtomicBool::new(false),
            rearms: AtomicU8::new(0),
            loaded: AtomicBool::new(false),
            announced: AtomicBool::new(false),
        }
    }

    /// Read the state and charge nothing: relaxed atomic loads, no lock, no game call. This is the
    /// half the guard can afford to ask on every load.
    pub fn plan(&self, attempts_cap: u8, bail_cap: u8) -> TableRead {
        if self.loaded.load(Ordering::Relaxed) {
            TableRead::Read
        }
        else if self.re_arm_if_stale(attempts_cap, bail_cap) {
            TableRead::Attempt
        }
        else if self.attempts.load(Ordering::Relaxed) < attempts_cap && self.lookups_without_a_key.load(Ordering::Relaxed) < bail_cap {
            TableRead::Attempt
        }
        else if self.watch_due(bail_cap) {
            TableRead::Attempt
        }
        else {
            // The cadence for the next key watch: every settled lookup counts here, and a watch is
            // granted once the count reaches the next doubled interval. Counting is one relaxed
            // read-modify-write, which is what AGENTS section 6 asks for on a per load path.
            self.settled_lookups.fetch_add(1, Ordering::Relaxed);
            self.announce_settled();
            TableRead::GiveUp
        }
    }

    /// Decide what this lookup gets to do **and charge it for, in the same call**, so the caps bound
    /// something the shipped path actually did instead of something a test chose to record.
    ///
    /// `key` - the game's database key, or nothing - is asked *once*, and only when a try is actually
    /// allowed, so the settled states stay atomic loads. A lookup that finds no key pays out of
    /// `bail_cap` and spends no open, which keeps a table that only becomes readable once the game has
    /// opened its own databases reachable. A lookup with a key in hand pays out of `attempts_cap`. A
    /// bail is still charged because it is the one lock this path takes without a table, and AGENTS
    /// section 6 does not accept a lock per asset load without an end: after `bail_cap` such lookups
    /// the window settles to `GiveUp` and every later lookup answers from atomic loads only.
    ///
    /// In the settled state the only thing on offer is a key watch, charged against
    /// `META_TABLE_KEY_WATCH_CAP`. A watch that finds no key answers `Unknown` and changes nothing. A
    /// watch that finds the key has witnessed the transition the re-arm is armed against, so this
    /// lookup - key in hand, window just re-opened - pays out of the re-armed `attempts_cap`.
    ///
    /// Honest about the shape: the counters are read, then charged, so lookups on other threads racing
    /// for the last slot can each take one more than the cap by the number of threads racing.
    /// Patched-asset loads walk this guard on the game thread (`LoadAsset_Internal` and
    /// `AssetBundleRequest::GetResult` both end in `on_LoadAsset`), which is the walk this bound is
    /// for.
    pub fn authorise(&self, attempts_cap: u8, bail_cap: u8, key: impl FnOnce() -> bool) -> TableStep {
        match self.plan(attempts_cap, bail_cap) {
            TableRead::Read => TableStep::Read,
            TableRead::GiveUp => TableStep::GiveUp,
            TableRead::Attempt => {
                let in_window = self.attempts.load(Ordering::Relaxed) < attempts_cap
                    && self.lookups_without_a_key.load(Ordering::Relaxed) < bail_cap;

                if in_window {
                    let has_key = key();
                    self.note_key_answer(has_key);

                    if has_key {
                        self.attempts.fetch_add(1, Ordering::Relaxed);
                        TableStep::Open
                    }
                    else {
                        self.lookups_without_a_key.fetch_add(1, Ordering::Relaxed);
                        TableStep::Bail
                    }
                }
                else {
                    self.key_watches.fetch_add(1, Ordering::Relaxed);
                    let has_key = key();
                    self.note_key_answer(has_key);

                    // The re-arm is what the watch exists for: a key found here, after this machine had
                    // already looked and found none, is the transition, and re-arming the window is the
                    // only thing witnessing it is allowed to buy. This lookup - key in hand, window just
                    // re-opened - then pays out of the re-armed `attempts_cap`. A key that was already
                    // in hand when the window armed is not a transition: it costs one key lock, spends
                    // no open, and re-opens nothing.
                    if has_key && self.re_arm_if_stale(attempts_cap, bail_cap) {
                        self.attempts.fetch_add(1, Ordering::Relaxed);
                        TableStep::Open
                    }
                    else {
                        TableStep::Watch
                    }
                }
            }
        }
    }

    /// Records what one key read answered, which is the only way this machine ever hears about the key:
    /// `RETRIEVED_RAW_KEY` is filled by `core::hachimi::sqlite3_key_hook` whenever the game keys one of
    /// its own databases, and nothing here polls it. Finding no key records the empty half of the
    /// transition; finding one *after* having found none moves this machine's generation forward exactly
    /// once, because a key that stays in the lock is one state change, not one per patched-asset load.
    ///
    /// A machine that never saw the lock empty is armed against a world that already had the key and
    /// witnesses nothing: no key read on its own re-opens a window.
    fn note_key_answer(&self, has_key: bool) {
        if !has_key {
            self.key_seen_absent.store(true, Ordering::Relaxed);
            return;
        }
        if !self.key_seen_absent.load(Ordering::Relaxed) {
            return;
        }
        if !self.key_seen_present.swap(true, Ordering::Relaxed) {
            self.generations_seen.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The re-opening half: a witnessed key generation this window was not armed against re-arms the
    /// window's two budgets, bounded by `META_TABLE_REARMS_CAP` so a settled state can be re-opened a
    /// fixed number of times and never in a loop.
    fn re_arm_if_stale(&self, attempts_cap: u8, bail_cap: u8) -> bool {
        let generation = self.generations_seen.load(Ordering::Relaxed);
        let armed = self.armed_generation.load(Ordering::Relaxed);

        if armed == generation {
            return false;
        }
        if self.armed_generation.compare_exchange(armed, generation, Ordering::Relaxed, Ordering::Relaxed).is_err() {
            // Another lookup already moved this window onto that generation.
            return false;
        }
        if self.rearms.fetch_add(1, Ordering::Relaxed) + 1 > META_TABLE_REARMS_CAP {
            // The generation is adopted either way: a window that may not re-arm again must not stay
            // stale, or every later lookup would walk back through this branch.
            return false;
        }

        self.attempts.store(0, Ordering::Relaxed);
        self.lookups_without_a_key.store(0, Ordering::Relaxed);
        self.settled_lookups.store(0, Ordering::Relaxed);
        self.announced.store(false, Ordering::Relaxed);
        self.announce_rearm(attempts_cap, bail_cap);
        true
    }

    /// A settled window's look for the key: `META_TABLE_KEY_WATCH_CAP` of them per machine, the n-th
    /// one due after `bail_cap << n` settled lookups, so the interval doubles (32, 64, 128, 256, 512,
    /// 1024) and the whole look costs six key locks across a session instead of one per load. The
    /// counter is deliberately not reset by a re-arm: the look is bounded per machine, not per window.
    fn watch_due(&self, bail_cap: u8) -> bool {
        let watches = self.key_watches.load(Ordering::Relaxed);
        if watches >= META_TABLE_KEY_WATCH_CAP {
            return false;
        }
        self.settled_lookups.load(Ordering::Relaxed) >= (bail_cap as u32) << watches
    }

    pub fn mark_loaded(&self) {
        self.loaded.store(true, Ordering::Relaxed);
    }

    /// The line that makes the bound readable in `hachimi.log` (AGENTS section 2: a change that
    /// cannot be shown in the log is not a change): written by the first lookup that finds a window
    /// settled and by none after it, until a re-arm clears the latch. Once written it costs one relaxed
    /// load. It says what the guard still does - apply, unconfirmed - because on a client whose `meta`
    /// table is not readable the settled answer must not read as a refusal.
    fn announce_settled(&self) {
        if !self.announced.load(Ordering::Relaxed) && !self.announced.swap(true, Ordering::Relaxed) {
            info!("Meta table identity tries spent: later patched-asset loads apply unconfirmed with no key lock, no database open and no name conversion");
        }
    }

    /// The line that makes the re-opening readable in the same log. Written once per re-arm, and
    /// `META_TABLE_REARMS_CAP` bounds how many there are.
    fn announce_rearm(&self, attempts_cap: u8, bail_cap: u8) {
        info!("Meta table identity tries re-armed: the game's sqlite key arrived, so the identity table may be readable now; later patched-asset loads get {} database opens and {} key locks again", attempts_cap, bail_cap);
    }
}

static META_TABLE_ATTEMPTS: TableAttempts = TableAttempts::new();

/// Times one armed window opens the game's `meta` database looking for the identity table. Charged by
/// `TableAttempts::authorise` when it hands out the open, so this bounds a `Connection::Open` and the
/// `error!` line that comes with it. `META_TABLE_REARMS_CAP` is what bounds the number of windows.
const META_TABLE_ATTEMPTS_CAP: u8 = 4;

/// Times one armed window takes the key lock on the way to that database before it settles to
/// `GiveUp`. Generous next to when the game opens its own databases, and it is what keeps that lock
/// bounded instead of a per load cost - charged by the same `authorise` call that grants it.
const META_TABLE_KEY_BAILS_CAP: u8 = 32;

/// Times a settled window spends one key lock looking for the key it was armed without, at the
/// doubling intervals `TableAttempts::watch_due` spaces them at. Six of them covers the span of a
/// session's patched-asset loads after a window settled, and it is what stops a key arriving late from
/// being invisible to a guard that had already given up on the table.
const META_TABLE_KEY_WATCH_CAP: u8 = 6;

/// Times one state machine re-arms a settled window against a key generation it witnessed. The key
/// transition is itself latched (`TableAttempts::note_key_answer`), so this is the hard backstop on the
/// re-opening: `(1 + META_TABLE_REARMS_CAP)` windows, at most `2 * META_TABLE_ATTEMPTS_CAP` opens and
/// `2 * META_TABLE_KEY_BAILS_CAP` key locks per machine.
const META_TABLE_REARMS_CAP: u8 = 1;

/// The one key read the shipped lookup makes, and the only place it takes that lock: the lock the
/// game's own `sqlite3_key` fills (`core::hachimi::sqlite3_key_hook`). The guard is released inside
/// this function, so the key lock is never held across `META_DATA` or across the game's sqlite open -
/// `sqlite3_open_v2_hook` reads this very lock, and a std `Mutex` is not reentrant.
///
/// It answers one question and nothing else: is a key in the lock. What the state machine makes of the
/// answer - whether this is the empty -> non-empty transition a window re-arms against - is
/// `TableAttempts::note_key_answer`'s. The key, its length and who may use it stay exactly as
/// `sqlite3_key_hook` wrote them (AGENTS section 2).
fn key_retrieved() -> bool {
    let key = RETRIEVED_RAW_KEY.lock().unwrap_or_else(|e| e.into_inner());
    !key.is_empty()
}

impl MetaData {
    /// What consulting the identity table costs the calling load right now, read without charging
    /// anything. A caller that gets `GiveUp` can answer `MetaIdentity::Unknown` without converting a
    /// name, taking a lock, calling into the game or writing a log line.
    pub fn table_plan() -> TableRead {
        META_TABLE_ATTEMPTS.plan(META_TABLE_ATTEMPTS_CAP, META_TABLE_KEY_BAILS_CAP)
    }

    /// The identity answer for one patched asset load.
    ///
    /// Cheap in the settled states: `Read` is one read lock and two map lookups on borrowed keys (no
    /// allocation), `GiveUp` is one atomic load and nothing else. The spending states are charged as
    /// they are granted: per armed window at most `META_TABLE_ATTEMPTS_CAP` database opens (which
    /// bounds that failure log by the same number) and `META_TABLE_KEY_BAILS_CAP` key locks, plus
    /// `META_TABLE_KEY_WATCH_CAP` key locks per process for the settled state's look for a key. After
    /// a window settles this function answers `MetaIdentity::Unknown` - an apply, not a refusal -
    /// until a witnessed key generation re-arms the window.
    pub fn identity_of(expected: &str, observed: &str) -> MetaIdentity {
        match META_TABLE_ATTEMPTS.authorise(META_TABLE_ATTEMPTS_CAP, META_TABLE_KEY_BAILS_CAP, key_retrieved) {
            TableStep::Read => {
                let meta = META_DATA.read().unwrap_or_else(|e| e.into_inner());
                meta.compare_identity(expected, observed)
            }
            // The answers that cost the caller nothing but a branch: a bail and a watch have both
            // already paid for the key lock they took, and a `GiveUp` took no lock at all.
            TableStep::Bail | TableStep::Watch | TableStep::GiveUp => MetaIdentity::Unknown,
            TableStep::Open => {
                let mut meta = META_DATA.write().unwrap_or_else(|e| e.into_inner());
                if meta.name_to_hash.is_empty() {
                    *meta = Self::load_from_db();
                }

                if meta.name_to_hash.is_empty() {
                    return MetaIdentity::Unknown;
                }

                META_TABLE_ATTEMPTS.mark_loaded();
                meta.compare_identity(expected, observed)
            }
        }
    }
}

fn get_single_column_int(sql: &str) -> Vec<i32> {
    let mut items = Vec::new();
    let db_path = get_masterdb_path();
    let conn = Connection::new();
    if Connection::Open(conn, db_path.to_il2cpp_string(), std::ptr::null_mut(), std::ptr::null_mut(), 0) {
        let query = Connection::Query(conn, sql.to_il2cpp_string());
        if !query.is_null() {
            while Query::Step(query) {
                items.push(Query::GetInt(query, 0));
            }
            Query::Dispose(query);
        }
        Connection::CloseDB(conn);
    }
    items
}

pub fn get_all_chara_ids() -> Vec<i32> {
    get_single_column_int("SELECT id FROM chara_data")
}

pub fn get_all_dress_ids() -> Vec<i32> {
    get_single_column_int("SELECT id FROM dress_data")
}

pub fn get_all_music_ids() -> Vec<i32> {
    get_single_column_int("SELECT music_id FROM live_data")
}

pub fn get_all_mob_ids() -> Vec<i32> {
    get_single_column_int("SELECT mob_id FROM mob_data WHERE use_live = 1")
}

pub fn get_default_dress_ids() -> Vec<i32> {
    get_single_column_int("SELECT id FROM dress_data WHERE (condition_type = 1 OR condition_type = 4 OR condition_type = 5) AND use_live_theater = 1 AND id < 999")
}

pub fn get_all_cards() -> Vec<(i32, i32)> {
    let mut items = Vec::new();
    let db_path = get_masterdb_path();
    let conn = Connection::new();
    if Connection::Open(conn, db_path.to_il2cpp_string(), std::ptr::null_mut(), std::ptr::null_mut(), 0) {
        let query = Connection::Query(conn, "SELECT id, default_rarity FROM card_data WHERE id <= 999999".to_il2cpp_string());
        if !query.is_null() {
            while Query::Step(query) {
                items.push((Query::GetInt(query, 0), Query::GetInt(query, 1)));
            }
            Query::Dispose(query);
        }
        Connection::CloseDB(conn);
    }
    items
}

pub fn get_master_text(category: i32, index: i32) -> Option<String> {
    let db_path = get_masterdb_path();
    let conn = Connection::new();
    if Connection::Open(conn, db_path.to_il2cpp_string(), std::ptr::null_mut(), std::ptr::null_mut(), 0) {
        let sql = format!("SELECT text FROM text_data WHERE \"category\" = {} AND \"index\" = {}", category, index);
        let query = Connection::Query(conn, sql.to_il2cpp_string());
        if !query.is_null() {
            if Query::Step(query) {
                let text_ptr = Query::GetText(query, 0);
                if let Some(text) = unsafe { text_ptr.as_ref() }.map(|s| s.as_utf16str().to_string()) {
                    Query::Dispose(query);
                    Connection::CloseDB(conn);
                    return Some(text);
                }
            }
            Query::Dispose(query);
        }
        Connection::CloseDB(conn);
    }
    None
}

pub fn get_jobs_info(reward_id: i32) -> Option<(i32, i32)> {
    let db_path = get_masterdb_path();
    let conn = Connection::new();
    if Connection::Open(conn, db_path.to_il2cpp_string(), std::ptr::null_mut(), std::ptr::null_mut(), 0) {
        let sql = format!("SELECT place_id, genre_id FROM jobs_reward WHERE \"id\" = {}", reward_id);
        let query = Connection::Query(conn, sql.to_il2cpp_string());
        if !query.is_null() {
            if Query::Step(query) {
                let place_id = Query::GetInt(query, 0);
                let genre_id = Query::GetInt(query, 1);
                Query::Dispose(query);
                Connection::CloseDB(conn);
                return Some((place_id, genre_id));
            }
            Query::Dispose(query);
        }
        Connection::CloseDB(conn);
    }
    None
}

pub fn get_jobs_place_race_track_id(place_id: i32) -> Option<i32> {
    let db_path = get_masterdb_path();
    let conn = Connection::new();
    if Connection::Open(conn, db_path.to_il2cpp_string(), std::ptr::null_mut(), std::ptr::null_mut(), 0) {
        let sql = format!("SELECT race_track_id FROM jobs_place WHERE \"id\" = {}", place_id);
        let query = Connection::Query(conn, sql.to_il2cpp_string());
        if !query.is_null() {
            if Query::Step(query) {
                let track_id = Query::GetInt(query, 0);
                Query::Dispose(query);
                Connection::CloseDB(conn);
                return Some(track_id);
            }
            Query::Dispose(query);
        }
        Connection::CloseDB(conn);
    }
    None
}

pub fn get_champions_resources() -> Vec<String> {
    let mut items = Vec::new();
    let db_path = get_masterdb_path();
    let conn = Connection::new();
    if Connection::Open(conn, db_path.to_il2cpp_string(), ptr::null_mut(), ptr::null_mut(), 0) {
        let sql = "SELECT t.text FROM champions_schedule c LEFT OUTER JOIN text_data t on t.category = 206 AND t.\"index\" = c.id GROUP BY c.resource_id";
        let query = Connection::Query(conn, sql.to_il2cpp_string());
        if !query.is_null() {
            while Query::Step(query) {
                let text_ptr = Query::GetText(query, 0);
                if let Some(text) = unsafe { text_ptr.as_ref() }.map(|s| s.as_utf16str().to_string()) {
                    items.push(text);
                } else {
                    items.push(rust_i18n::t!("unknown").into_owned());
                }
            }
            Query::Dispose(query);
        }
        Connection::CloseDB(conn);
    }
    items
}

pub fn get_champions_live_max_year() -> i32 {
    let mut max_year = Utc::now().year(); // fallback to the current year since it's guaranteed to have textures
    if !SceneManager::is_home_init() { return max_year; }
    let db_path_str = get_meta_path();

    let conn = Connection::new();
    if Hachimi::instance().game.region == Region::Japan {
        AUTO_UNLOCK_NEXT_DB.store(true, Ordering::Relaxed);
    }
    if Connection::Open(conn, db_path_str.to_il2cpp_string(), ptr::null_mut(), ptr::null_mut(), 0) {
        let sql = "SELECT n FROM a WHERE n LIKE 'live/image/champions/tex_championslive_year_%'";
        let query = Connection::Query(conn, sql.to_il2cpp_string());

        if !query.is_null() {
            let mut max_idx = -1;
            while Query::Step(query) {
                let text_ptr = Query::GetText(query, 0);
                if let Some(text) = unsafe { text_ptr.as_ref() }.map(|s| s.as_utf16str().to_string()) {
                    if let Some(idx_str) = text.strip_prefix("live/image/champions/tex_championslive_year_") {
                        if let Ok(idx) = idx_str.parse::<i32>() {
                            max_idx = max_idx.max(idx);
                        }
                    }
                }
            }
            Query::Dispose(query);
            if max_idx >= 0 {
                max_year = 2022 + max_idx;
            }
        }
        Connection::CloseDB(conn);
    }
    max_year
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// The shapes C10 actually has on the primary target, measured rather than invented: a Global
    /// install stores what it downloads as `<Persistent>/dat/<2 chars>/<32 char id>` - 171,845 files,
    /// none of them with an extension - and the identity a shipped data package records for
    /// `assets/atlas/common` is `ZVG5NHCU7HGSYCCXSJOTUGAE5Q2UHJKD`. The `n` column is `/` separated
    /// and extension-less, which is the shape `get_champions_live_max_year` above parses as
    /// `live/image/champions/tex_championslive_year_<i>` and turns into a year.
    fn meta_table() -> MetaData {
        let mut meta = MetaData::default();
        meta.record("atlas/common", "ZVG5NHCU7HGSYCCXSJOTUGAE5Q2UHJKD");
        meta.record("atlas/common_v2", "222H4K4VZG6BTBWHDB6BVUJHW5LLNT7E");
        meta.record("live/image/champions/tex_championslive_year_3", "IZ2K5DI3UXADWQERBTGA2RZRAVWUNUHS");
        meta
    }

    /// The old key was `format!("{}.a", last component of n)`. The lookup half handed it a bare
    /// `file_name()`, so the two halves could never agree and no identity was ever resolved - on any
    /// region.
    #[test]
    fn the_table_is_indexed_under_the_names_it_actually_holds() {
        let meta = meta_table();

        assert_eq!(meta.hash_for("atlas/common"), Some("ZVG5NHCU7HGSYCCXSJOTUGAE5Q2UHJKD"));
        assert_eq!(meta.hash_for("common"), Some("ZVG5NHCU7HGSYCCXSJOTUGAE5Q2UHJKD"));
        // A client that reports the component with an extension still reaches the same row.
        assert_eq!(meta.hash_for("res/ui/atlas/common.a"), Some("ZVG5NHCU7HGSYCCXSJOTUGAE5Q2UHJKD"));
        assert_eq!(meta.hash_for("res\\ui\\atlas\\common"), Some("ZVG5NHCU7HGSYCCXSJOTUGAE5Q2UHJKD"));
        assert_eq!(meta.hash_for("tex_championslive_year_3"), Some("IZ2K5DI3UXADWQERBTGA2RZRAVWUNUHS"));
        assert_eq!(meta.hash_for("ZVG5NHCU7HGSYCCXSJOTUGAE5Q2UHJKD"), None, "the id is not a name the table indexes by");
    }

    /// The invented extension is gone from the key set entirely: a row whose `n` has no extension
    /// must not become reachable only under a name that client never reports.
    #[test]
    fn no_key_carries_an_extension_the_a_table_does_not_have() {
        let meta = meta_table();
        assert!(meta.name_to_hash.contains_key("atlas/common"));
        assert!(meta.name_to_hash.contains_key("common"));
        assert!(!meta.name_to_hash.keys().any(|key| key.ends_with(".a")),
            "a key built as `{{name}}.a` is only reachable if some client reports the bundle under exactly that name");
    }

    #[test]
    fn a_recorded_id_this_table_records_for_a_bundle_is_the_same_bundle() {
        let meta = meta_table();
        assert_eq!(meta.compare_identity("ZVG5NHCU7HGSYCCXSJOTUGAE5Q2UHJKD", "atlas/common"), MetaIdentity::Same);
        assert_eq!(meta.compare_identity("ZVG5NHCU7HGSYCCXSJOTUGAE5Q2UHJKD", "common"), MetaIdentity::Same);
        assert_eq!(meta.compare_identity("ZVG5NHCU7HGSYCCXSJOTUGAE5Q2UHJKD", "res/ui/atlas/common.a"), MetaIdentity::Same);
    }

    /// The #56 case: both ids are rows in this client's own table and they name different bundles.
    #[test]
    fn two_ids_this_client_names_as_two_different_bundles_are_a_mismatch() {
        let meta = meta_table();
        assert_eq!(
            meta.compare_identity("222H4K4VZG6BTBWHDB6BVUJHW5LLNT7E", "atlas/common"),
            MetaIdentity::Different {
                expected_logical: "atlas/common_v2".to_owned(),
                observed_logical: "atlas/common".to_owned(),
            }
        );
    }

    /// A recorded id this table has no row for - the id the authoring install has for a bundle whose
    /// contents differ per package, or an id from a version this install no longer has - names
    /// nothing here. That is not evidence the bundle in front of the guard is the wrong one.
    #[test]
    fn a_recorded_id_this_table_has_no_row_for_decides_nothing() {
        let meta = meta_table();
        assert_eq!(meta.compare_identity("2d8f1a3b9c0d1e2f3a4b5c6d7e8f9012", "atlas/common"), MetaIdentity::Unknown);
        assert_eq!(meta.compare_identity("ZVG5NHCU7HGSYCCXSJOTUGAE5Q2UHJKD", "atlas/photostudio"), MetaIdentity::Unknown,
            "a bundle the table does not name is not a bundle this client can call wrong");
    }

    /// The caps are only caps if the code that spends them pays for them, so every test below drives
    /// `TableAttempts::authorise` - the shipped grant - or the shipped pair `MetaData::table_plan` +
    /// `MetaData::identity_of`, and reads what the state machine charged. There is no counting method
    /// left for a test to feed.
    ///
    /// A test never reaches `MetaData::load_from_db`, and that is deliberate: its `Hachimi::instance()`
    /// ends the process when the game is not running (AGENTS section 4), so the tests drive the shipped
    /// *grant* - which decides an open and charges for it - rather than executing an open a test
    /// process has no game sqlite to serve.

    /// The fact a lookup reports to the state machine: a key the game already used is in the lock.
    /// Reporting it is not the same as the game handing one over, and `TableAttempts` is built not to
    /// treat it as one: a machine that never saw the lock empty witnesses no transition.
    fn key_in_hand() -> bool { true }

    /// The half one armed window pays at most `META_TABLE_ATTEMPTS_CAP` times. The grant is what bounds
    /// the open: `MetaData::identity_of`'s `TableStep::Open` arm is the only place that calls
    /// `Self::load_from_db` (and so `Connection::Open`), and it only gets there through this grant -
    /// four grants are four opens, and four `error!` lines at most.
    ///
    /// A table that could not be read used to look exactly like a table that had not been read yet, so
    /// every later lookup on a per asset load path re-ran the write lock, `get_meta_path()`,
    /// `Connection::new`, `Connection::Open`, a managed string allocation and an `error!` line (AGENTS
    /// section 6 keeps all of those off it).
    ///
    /// A key in the lock from the first lookup is also the case that must *not* re-arm: nothing moved,
    /// so the re-arm budget stays untouched and the open bound is the one window it was.
    #[test]
    fn the_meta_database_is_opened_a_bounded_number_of_times_per_process() {
        assert_eq!(META_TABLE_ATTEMPTS_CAP, 4, "the shipped open bound is the number this item records");
        assert_eq!(META_TABLE_KEY_WATCH_CAP, 6, "the settled state's key look is this many key locks");
        assert_eq!(META_TABLE_REARMS_CAP, 1, "one re-armed window per machine is the bound this item records");
        let attempts = TableAttempts::new();

        // 2048 loads: long enough to spend the window and every key watch the cadence grants after it
        // (32 + 32 + 64 + 128 + 256 + 512 settled lookups apart), so the settled state is fully drained.
        let mut opens = 0;
        for _ in 0..2048 {
            if attempts.authorise(META_TABLE_ATTEMPTS_CAP, META_TABLE_KEY_BAILS_CAP, key_in_hand) == TableStep::Open {
                opens += 1;
            }
        }

        assert_eq!(opens, META_TABLE_ATTEMPTS_CAP as usize, "2048 patched asset loads reached the meta database open once per load instead of once per bounded try");
        assert_eq!(attempts.attempts.load(Ordering::Relaxed), 4);
        assert_eq!(attempts.lookups_without_a_key.load(Ordering::Relaxed), 0, "a lookup that had a key in hand is not a bail");
        assert_eq!(attempts.key_watches.load(Ordering::Relaxed), META_TABLE_KEY_WATCH_CAP, "the settled state's key look is bounded, and it was spent");
        assert_eq!(attempts.rearms.load(Ordering::Relaxed), 0, "a key that was in the lock all along is not a state change, so nothing re-armed");
        assert_eq!(attempts.plan(META_TABLE_ATTEMPTS_CAP, META_TABLE_KEY_BAILS_CAP), TableRead::GiveUp);

        // And the bound holds over a longer session than this one: the drained machine grants no open
        // for the next 2048 loads either.
        for _ in 0..2048 {
            assert_ne!(attempts.authorise(META_TABLE_ATTEMPTS_CAP, META_TABLE_KEY_BAILS_CAP, key_in_hand), TableStep::Open,
                "a drained machine kept handing out opens");
        }
        assert_eq!(opens, META_TABLE_ATTEMPTS_CAP as usize);
        assert_eq!(attempts.key_watches.load(Ordering::Relaxed), META_TABLE_KEY_WATCH_CAP);
    }

    /// The half one armed window pays at most `META_TABLE_KEY_BAILS_CAP` times: a lookup that finds no
    /// key spends the key lock and nothing else, so a table that only becomes readable after the game
    /// has opened its own databases stays reachable - until the window's tries are spent, and then for
    /// at most `META_TABLE_KEY_WATCH_CAP` further key locks.
    ///
    /// `key_locks` counts every entry into the key lock from *outside* the state machine, so the bound
    /// is measured rather than read back out of the counter the test itself filled.
    #[test]
    fn a_lookup_with_no_key_yet_spends_no_open_and_bails_out_after_a_bounded_number_of_locks() {
        assert_eq!(META_TABLE_KEY_BAILS_CAP, 32, "the shipped key-bail bound is the number this item records");
        let attempts = TableAttempts::new();
        let key_locks = Cell::new(0usize);
        let no_key_yet = || { key_locks.set(key_locks.get() + 1); false }; // what this install reports before the game opens its own databases

        let mut bails = 0;
        let mut watches = 0;
        let mut settled = 0;
        for _ in 0..2048 {
            match attempts.authorise(META_TABLE_ATTEMPTS_CAP, META_TABLE_KEY_BAILS_CAP, no_key_yet) {
                TableStep::Bail => bails += 1,
                TableStep::Watch => watches += 1,
                TableStep::GiveUp => settled += 1,
                step => unreachable!("a window that never had a key handed out {:?}", step),
            }
        }

        assert_eq!(bails, META_TABLE_KEY_BAILS_CAP as usize, "the window's key-bail budget is the number this item records");
        assert_eq!(watches, META_TABLE_KEY_WATCH_CAP as usize, "the settled state's look for a key is bounded too");
        assert_eq!(settled, 2048 - (META_TABLE_KEY_BAILS_CAP + META_TABLE_KEY_WATCH_CAP) as usize,
            "every later lookup answered from atomic loads, which is the whole point of the bound");
        assert_eq!(key_locks.get(), (META_TABLE_KEY_BAILS_CAP + META_TABLE_KEY_WATCH_CAP) as usize,
            "the key lock was taken once per patched asset load instead of a bounded number of times");
        assert_eq!(attempts.attempts.load(Ordering::Relaxed), 0, "a bail or a watch that opened nothing must not spend an open");
        assert_eq!(attempts.key_seen_absent.load(Ordering::Relaxed), true, "the empty half of the key transition is what these lookups recorded");
        assert_eq!(attempts.generations_seen.load(Ordering::Relaxed), 0, "no key ever arrived, so no generation moved");
        assert_eq!(attempts.plan(META_TABLE_ATTEMPTS_CAP, META_TABLE_KEY_BAILS_CAP), TableRead::GiveUp);
    }

    /// The Japan shape: the game keys one of its own databases part way into the session, and the
    /// lookup that first finds the key is a **window** lookup, not a watch. The transition re-opens the
    /// window exactly once - the lookups it had already spent key-less are not paid for twice - and
    /// everything after it is bounded: the re-armed window's opens, the watch budget, then `GiveUp` for
    /// the rest of the session.
    #[test]
    fn a_key_arriving_mid_session_re_opens_the_window_once_and_no_more() {
        let attempts = TableAttempts::new();
        let key_locks = Cell::new(0usize);
        let arrived_at = 10usize; // the game hands its key over this many patched-asset loads in

        let mut opens = 0;
        let mut bails = 0;
        let mut watches = 0;
        let mut settled = 0;
        for i in 0..4096usize {
            let key_now = || { key_locks.set(key_locks.get() + 1); i >= arrived_at };
            match attempts.authorise(META_TABLE_ATTEMPTS_CAP, META_TABLE_KEY_BAILS_CAP, key_now) {
                TableStep::Open => opens += 1,
                TableStep::Bail => bails += 1,
                TableStep::Watch => watches += 1,
                TableStep::GiveUp => settled += 1,
                TableStep::Read => unreachable!("nothing here ever loads the table"),
            }
        }

        assert_eq!(bails, arrived_at, "the key-less lookups before the arrival are charged as bails");
        assert_eq!(opens, META_TABLE_ATTEMPTS_CAP as usize + 1, "one open was spent before the arrival, then the re-armed window's four");
        assert_eq!(watches, META_TABLE_KEY_WATCH_CAP as usize, "and the settled state's key look ran out");
        assert_eq!(settled, 4096 - (arrived_at + META_TABLE_ATTEMPTS_CAP as usize + 1 + META_TABLE_KEY_WATCH_CAP as usize));
        assert_eq!(key_locks.get(), arrived_at + META_TABLE_ATTEMPTS_CAP as usize + 1 + META_TABLE_KEY_WATCH_CAP as usize,
            "the key lock was taken once per patched asset load instead of a bounded number of times");
        assert_eq!(attempts.rearms.load(Ordering::Relaxed), META_TABLE_REARMS_CAP, "the transition happened once, so the window re-armed once");
        assert_eq!(attempts.generations_seen.load(Ordering::Relaxed), 1, "the key staying in the lock is not a new generation per load");
        assert_eq!(attempts.attempts.load(Ordering::Relaxed), META_TABLE_ATTEMPTS_CAP);
        assert_eq!(attempts.plan(META_TABLE_ATTEMPTS_CAP, META_TABLE_KEY_BAILS_CAP), TableRead::GiveUp,
            "a session that keeps loading patched assets ends settled, not re-arming");
    }

    /// Once the table is in memory the tries are not consulted again, the read path is the only one
    /// left, and it does not touch the key lock at all.
    #[test]
    fn a_loaded_table_is_read_without_spending_any_more_attempts() {
        let attempts = TableAttempts::new();
        assert_eq!(attempts.authorise(META_TABLE_ATTEMPTS_CAP, META_TABLE_KEY_BAILS_CAP, key_in_hand), TableStep::Open);
        attempts.mark_loaded();

        let key_locks = Cell::new(0usize);
        for _ in 0..500 {
            assert_eq!(attempts.authorise(META_TABLE_ATTEMPTS_CAP, META_TABLE_KEY_BAILS_CAP, || { key_locks.set(key_locks.get() + 1); true }), TableStep::Read);
        }

        assert_eq!(key_locks.get(), 0, "the settled read state never asks the key lock");
        assert_eq!(attempts.attempts.load(Ordering::Relaxed), 1);
        assert_eq!(attempts.key_watches.load(Ordering::Relaxed), 0, "a table in memory is never settled, so it never watches");
    }

    /// C10's cost state on the shipped path, end to end, in both directions.
    ///
    /// The drive is the exact pair `AssetBundle::check_asset_bundle_name` runs: `MetaData::table_plan()`
    /// to decide whether the table is worth asking, the one name conversion, then
    /// `MetaData::identity_of`. Nothing in a test process ever runs the game's `sqlite3_key`, so
    /// `RETRIEVED_RAW_KEY` starts empty - the state C10 measured on this Global install, where
    /// `<Persistent>/meta` is not `SQLite format 3` and an unkeyed open reads no rows.
    ///
    /// What the old shape of this test could not see: `identity_of`'s bail returned `Unknown` without
    /// paying for the key lock it had just taken, so `plan` answered `Attempt` forever,
    /// `META_TABLE_KEY_BAILS_CAP` was never reached and the guard's `GiveUp` skip never fired. What the
    /// fix for *that* left open: the settled state had no way back out, and because a patch whose name
    /// matches the bundle returns before any charge, the whole 32-lock budget belongs to the mismatched
    /// loads the table exists to arbitrate - so a client whose sqlite key arrives late settled into
    /// answering `Unknown` for the rest of the session. Both halves are asserted here: it settles, and
    /// the key arriving re-opens it.
    ///
    /// Only this test touches the process-wide state machine and the shared key lock, so its numbers are
    /// its own.
    #[test]
    fn a_settled_guard_pays_for_the_lock_it_takes_and_a_key_arriving_late_un_settles_it() {
        assert_eq!(META_TABLE_KEY_BAILS_CAP, 32, "the shipped key-bail bound is the number this item records");
        assert!(RETRIEVED_RAW_KEY.lock().unwrap_or_else(|e| e.into_inner()).is_empty(),
            "this test starts in the no-key state; a key in the shared lock would take the game's sqlite open");

        let expected = "ZVG5NHCU7HGSYCCXSJOTUGAE5Q2UHJKD";
        let observed = "atlas/common";

        // 1. The window spends its key bails: every one of them charged by the shipped grant, one name
        //    conversion each, nothing opened.
        let mut name_conversions = 0;
        for _ in 0..META_TABLE_KEY_BAILS_CAP {
            assert_eq!(MetaData::table_plan(), TableRead::Attempt);
            name_conversions += 1;
            assert_eq!(MetaData::identity_of(expected, observed), MetaIdentity::Unknown);
        }
        assert_eq!(name_conversions, META_TABLE_KEY_BAILS_CAP as usize,
            "the guard converted a bundle name once per patched asset load instead of a bounded number of times");
        assert_eq!(META_TABLE_ATTEMPTS.lookups_without_a_key.load(Ordering::Relaxed), META_TABLE_KEY_BAILS_CAP,
            "the key-bail counter only moved where a test put it: the shipped bail never paid for the lock it took");
        assert_eq!(META_TABLE_ATTEMPTS.attempts.load(Ordering::Relaxed), 0,
            "a bail must not spend an open, and must never reach `Connection::Open`");

        // 2. It settles: the guard's skip, and the settled answer charges nothing.
        for _ in 0..(META_TABLE_KEY_BAILS_CAP - 1) {
            assert_eq!(MetaData::table_plan(), TableRead::GiveUp, "the guard is skipping the table, as it must");
        }
        assert_eq!(MetaData::identity_of(expected, observed), MetaIdentity::Unknown, "the settled state still answers");
        assert_eq!(META_TABLE_ATTEMPTS.lookups_without_a_key.load(Ordering::Relaxed), META_TABLE_KEY_BAILS_CAP,
            "the settled state charges nothing more");
        assert_eq!(META_TABLE_ATTEMPTS.key_watches.load(Ordering::Relaxed), 0);

        // 3. The settled state's bounded look: the first key watch comes at `bail_cap` settled lookups,
        //    and a watch that finds no key is charged as a watch.
        assert_eq!(MetaData::table_plan(), TableRead::Attempt, "the cadence granted the settled state its first key watch");
        assert_eq!(MetaData::identity_of(expected, observed), MetaIdentity::Unknown, "a watch with an empty lock answers Unknown");
        assert_eq!(META_TABLE_ATTEMPTS.key_watches.load(Ordering::Relaxed), 1);
        assert_eq!(META_TABLE_ATTEMPTS.lookups_without_a_key.load(Ordering::Relaxed), META_TABLE_KEY_BAILS_CAP,
            "a watch is charged as a watch, not as another bail");

        // 4. The observable state change: the game keys one of its own databases and
        //    `sqlite3_key_hook` writes the key into the same lock. Nothing notices it until a shipped
        //    key read looks.
        *RETRIEVED_RAW_KEY.lock().unwrap_or_else(|e| e.into_inner()) = vec![b'7'; 32];
        assert_eq!(META_TABLE_ATTEMPTS.generations_seen.load(Ordering::Relaxed), 0,
            "the key is only witnessed by the lookup that actually reads it");

        let mut settled = 0;
        while MetaData::table_plan() == TableRead::GiveUp {
            settled += 1;
        }
        assert_eq!(settled, META_TABLE_KEY_BAILS_CAP as usize, "the next key watch is spaced at 2 * bail_cap settled lookups");

        // The shipped grant, on the shipped key read. `identity_of` is not called here because the step
        // it would act on is an open, and a test process has no game sqlite to serve one.
        let step = META_TABLE_ATTEMPTS.authorise(META_TABLE_ATTEMPTS_CAP, META_TABLE_KEY_BAILS_CAP, key_retrieved);
        assert_eq!(step, TableStep::Open, "the watch found the key the window was armed without, and the re-armed window funds the open it exists to fund");
        assert_eq!(META_TABLE_ATTEMPTS.generations_seen.load(Ordering::Relaxed), 1, "empty -> non-empty was witnessed as one generation");
        assert_eq!(META_TABLE_ATTEMPTS.armed_generation.load(Ordering::Relaxed), 1, "the window is armed against the key it now has");
        assert_eq!(META_TABLE_ATTEMPTS.rearms.load(Ordering::Relaxed), 1);
        assert_eq!(META_TABLE_ATTEMPTS.attempts.load(Ordering::Relaxed), 1, "the open is charged by the same call that granted it");
        assert_eq!(META_TABLE_ATTEMPTS.lookups_without_a_key.load(Ordering::Relaxed), 0,
            "un-settled: the 32 key-less lookups the window spent are tries again");
        assert_eq!(META_TABLE_ATTEMPTS.settled_lookups.load(Ordering::Relaxed), 0);
        assert_eq!(MetaData::table_plan(), TableRead::Attempt, "the guard asks the table again instead of answering from the settled state");

        // 5. One re-arm, not a loop. The key is still in the lock on every later lookup and it is no
        //    longer a state change: the re-armed window funds its remaining opens, then settles again and
        //    the watch cadence runs out.
        let mut opens = 0;
        for _ in 0..500 {
            if META_TABLE_ATTEMPTS.authorise(META_TABLE_ATTEMPTS_CAP, META_TABLE_KEY_BAILS_CAP, key_retrieved) == TableStep::Open {
                opens += 1;
            }
        }
        assert_eq!(opens, (META_TABLE_ATTEMPTS_CAP - 1) as usize, "the re-armed window funds its remaining opens and no more");
        assert_eq!(META_TABLE_ATTEMPTS.rearms.load(Ordering::Relaxed), META_TABLE_REARMS_CAP, "one re-armed window per machine: a key already in hand re-opens nothing a second time");
        assert_eq!(META_TABLE_ATTEMPTS.generations_seen.load(Ordering::Relaxed), 1, "the transition is latched, not per load");
        assert_eq!(META_TABLE_ATTEMPTS.attempts.load(Ordering::Relaxed), META_TABLE_ATTEMPTS_CAP);

        for _ in 0..4096 {
            assert_ne!(META_TABLE_ATTEMPTS.authorise(META_TABLE_ATTEMPTS_CAP, META_TABLE_KEY_BAILS_CAP, key_retrieved), TableStep::Open,
                "the second window settled and it kept handing out opens");
        }
        assert_eq!(opens, (META_TABLE_ATTEMPTS_CAP - 1) as usize);
        assert_eq!(META_TABLE_ATTEMPTS.key_watches.load(Ordering::Relaxed), META_TABLE_KEY_WATCH_CAP, "the watches ran out, so the settled state stays settled");
        assert_eq!(MetaData::table_plan(), TableRead::GiveUp);

        // Leave the shared key lock as this test found it: `sqlite3_key_hook` only ever fills an empty
        // lock, and no later test should read a key this test invented.
        *RETRIEVED_RAW_KEY.lock().unwrap_or_else(|e| e.into_inner()) = Vec::new();
    }
}
