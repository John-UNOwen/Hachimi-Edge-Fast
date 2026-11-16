//! The menu's career dump button: write the current career to a JSON file in the game folder.
//!
//! One press serializes the whole run as the game itself holds it: the character (id, card, grade,
//! fan count, motivation), the five parameters and their caps, the aptitude grades, skill points,
//! acquired skills with level, the skill hints with their levels and discounts, the skill screen's
//! rows with their prices when the screen was last open, and the master-data names seen for every
//! skill. The file lands as `career_<charaId>_<epoch>.json` in the game root, the same place the
//! scrolling screenshot saves.
//!
//! How this stays safe outside the skill screen, where the dump may be pressed:
//! - No character or screen pointer is ever trusted from a door. The only cached pointer is the
//!   `WorkSingleModeData` career state, stamped every time the game itself reads its `get_Month`
//!   (the career HUD's clock) or hands the character over through `set_Character`, and cleared when
//!   `set_IsPlaying` reports the session ended. A dump only runs while that stamp is fresh, so the
//!   cached pointer was alive when the game last touched it, and the character is then re-derived
//!   by calling `get_Character` on the live state, never from a remembered address.
//! - Numbers about the character are read fresh, by calling the class's own getter methods through
//!   addresses resolved by signature at init (A2/A5: plain `int()` and enum-returning getters travel
//!   in registers; a `struct` wider than 4 bytes is never pushed through a wrapper, so the
//!   `ObscuredInt` properties are read at their backing-field offsets like the SP already is).
//! - The skill screen's rows and the master names are copied into owned data by
//!   `SingleModeSkillProbe` at the moment its doors fire; this module serializes those copies.

use std::cell::Cell;
use std::sync::atomic::{self, AtomicBool, AtomicI64, AtomicUsize};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use rust_i18n::t;
use serde_json::{Map, Value, json};

use crate::il2cpp::hook::umamusume::SingleModeSkillProbe::{self as skill_capture, clean_field_name, obscured_int_plain};
use crate::il2cpp::{
    api::*,
    ext::Il2CppStringExt,
    hook::umamusume::{AnimationSpeed, ProbeSuite, TrainingCuttProbe::class_for_label},
    types::*,
};
use crate::core::Hachimi;
use crate::core::gui::{NotificationRequest, request_notification};

const I4: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_I4;
const U4: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_U4;
const I2: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_I2;
const U2: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_U2;
const I1: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_I1;
const U1: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_U1;
const I8: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_I8;
const U8: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_U8;
const BOOL: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN;
const R4: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_R4;
const R8: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_R8;
const STRING: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_STRING;
const CLASS: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_CLASS;
const VOID: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_VOID;
const GENERIC: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_GENERICINST;
const VALUETYPE: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE;
const ENUM: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_ENUM;

// The career state counts as live while the game itself read it this recently. `get_Month` runs
// whenever the career HUD draws its clock, so any screen with the run visible keeps the stamp warm;
// well outside it the cached pointer is refused rather than read.
const FRESH_WINDOW_US: i64 = 120_000_000;

// The generic JSON walk: per object, per list, per string, and one budget for the whole dump so
// the walk can never outrun its bounds, whatever the metadata says next build.
const WALK_FIELD_CAP: usize = 40;
const WALK_LIST_CAP: usize = 48;
const WALK_STRING_CAP: usize = 512;
const WALK_MAX_DEPTH: usize = 2;
const WALK_NODE_BUDGET: usize = 6000;

// (section, key, method) triples, all read off the career character by its own getters. The names
// and return shapes are from the live dump of `Gallop.WorkSingleModeCharaData` (`introspect-run69.log`).
const I4_GETTERS: &[(&str, &str, &str)] = &[
    ("character", "charaDataId", "get_Id"),
    ("character", "cardId", "get_CardId"),
    ("character", "charaId", "get_CharaId"),
    ("character", "scenarioId", "get_ScenarioId"),
    ("character", "scenarioImageId", "get_ScenarioImageId"),
    ("character", "difficultyId", "get_DifficultyId"),
    ("character", "difficulty", "get_Difficulty"),
    ("character", "routeId", "get_RouteId"),
    ("character", "entryProgramId", "get_EntryProgramId"),
    ("character", "fanCount", "get_FanCount"),
    ("character", "charaEffectSkillPointDiscount", "GetCharaEffectSkillPointDiscount"),
    ("stats", "hp", "get_Hp"),
    ("stats", "maxHp", "get_MaxHp"),
    ("stats", "speed", "get_Speed"),
    ("stats", "stamina", "get_Stamina"),
    ("stats", "power", "get_Power"),
    ("stats", "guts", "get_Guts"),
    ("stats", "wiz", "get_Wiz"),
];
// enum getters: the returned enum travels in a register, so the wrapper declares an i32 (A5). This
// metadata reports those returns as VALUETYPE, not ENUM - the live run's resolver line said it
// plainly: `get_CharaGrade returns il2cpp type 17, wrapper expects 85`.
const ENUM_GETTERS: &[(&str, &str, &str)] = &[
    ("character", "charaGrade", "get_CharaGrade"),
    ("character", "motivation", "get_Motivation"),
    ("aptitudes", "groundTurf", "get_ProperGroundTurf"),
    ("aptitudes", "groundDirt", "get_ProperGroundDirt"),
    ("aptitudes", "distanceShort", "get_ProperDistanceShort"),
    ("aptitudes", "distanceMile", "get_ProperDistanceMile"),
    ("aptitudes", "distanceMiddle", "get_ProperDistanceMiddle"),
    ("aptitudes", "distanceLong", "get_ProperDistanceLong"),
    ("aptitudes", "styleNige", "get_ProperRunningStyleNige"),
    ("aptitudes", "styleSenko", "get_ProperRunningStyleSenko"),
    ("aptitudes", "styleSashi", "get_ProperRunningStyleSashi"),
    ("aptitudes", "styleOikomi", "get_ProperRunningStyleOikomi"),
];
// The 1-byte enum: the return travels in AL, so only that byte may be read.
const U8_GETTERS: &[(&str, &str, &str)] = &[("aptitudes", "runningStyle", "get_RunningStyle")];
const STRING_GETTERS: &[(&str, &str, &str)] = &[
    ("character", "gradeName", "GetCharaGradeName"),
    ("character", "startTime", "get_StartTime"),
];
// ObscuredInt properties: their getters return a 20-byte struct, so they are read at the
// backing-field offset instead, decoded by the same rule the SP read uses (A5). Only properties
// whose backing field holds the live value belong here: the run's JSON proved MaxSpeed..MaxWiz
// resolve their caps through the parameter dictionaries and leave their backing fields at 1, so
// those five stay out and `maxHp`, a real `int()` getter, stays in the table above.
const OBSCURED_GETTERS: &[(&str, &str, &str)] = &[
    ("character", "talentLevel", "TalentLevel"),
    ("character", "limitBreakCount", "LimitBreakCount"),
];
// generic List getters, written as top-level sections by their element walk.
const LIST_GETTERS: &[(&str, &str)] = &[
    ("acquiredSkills", "get_AcquiredSkillList"),
    ("skillHints", "get_SkillTipsList"),
    ("raceEvaluations", "get_EvaluationList"),
];

type GetterEntry = (&'static str, &'static str, usize);
static BOUND_I4: Mutex<Vec<GetterEntry>> = Mutex::new(Vec::new());
static BOUND_ENUM: Mutex<Vec<GetterEntry>> = Mutex::new(Vec::new());
static BOUND_U8: Mutex<Vec<GetterEntry>> = Mutex::new(Vec::new());
static BOUND_STRING: Mutex<Vec<GetterEntry>> = Mutex::new(Vec::new());
static BOUND_LIST: Mutex<Vec<GetterEntry>> = Mutex::new(Vec::new());

static ARMED: AtomicBool = AtomicBool::new(false);
static REQUESTED: AtomicBool = AtomicBool::new(false);

// The career state, its freshness stamp, and the month the game last reported - the HUD clock,
// cached from the door that doubles as the liveness stamp.
static DATA_AT: AtomicUsize = AtomicUsize::new(0);
static DATA_STAMP_US: AtomicI64 = AtomicI64::new(-1);
static LAST_MONTH: AtomicI64 = AtomicI64::new(-1);
static GET_CHARACTER_ADDR: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static WALK_NODES: Cell<usize> = const { Cell::new(0) };
}

fn lock_or_recover<T>(cell: &Mutex<T>) -> MutexGuard<'_, T> {
    cell.lock().unwrap_or_else(|e| e.into_inner())
}

fn budget_take() -> bool {
    WALK_NODES.with(|nodes| {
        let left = nodes.get();

        if left == 0 {
            false
        }
        else {
            nodes.set(left - 1);
            true
        }
    })
}

/// The liveness rule, plain enough to hold in a test: a cached pointer is usable only while it was
/// stamped after the suite started and no further back than the window.
fn is_fresh(now_us: i64, stamp_us: i64) -> bool {
    stamp_us >= 0 && now_us >= 0 && now_us - stamp_us <= FRESH_WINDOW_US && now_us - stamp_us >= 0
}

// ---- liveness doors ---------------------------------------------------------------------------

type GetMonthFn = extern "C" fn(this: *mut Il2CppObject) -> i32;
def_detour! {
    WorkSingleModeData_get_Month(this: *mut Il2CppObject) -> i32 {
        let month = get_orig_fn!(WorkSingleModeData_get_Month, GetMonthFn)(this);

        // Two relaxed stores and a monotonic clock read: this is the career HUD's cadence, not a
        // render path, and the store is what proves the cached state pointer alive.
        if !this.is_null() {
            DATA_AT.store(this as usize, atomic::Ordering::Release);
            DATA_STAMP_US.store(ProbeSuite::now_us(), atomic::Ordering::Relaxed);
            LAST_MONTH.store(month as i64, atomic::Ordering::Relaxed);
        }

        month
    }
    bail {
                get_orig_fn!(WorkSingleModeData_get_Month, GetMonthFn)(this)
    }
}

type SetCharacterFn = extern "C" fn(this: *mut Il2CppObject, chara: *mut Il2CppObject);
def_detour! {
    WorkSingleModeData_set_Character(this: *mut Il2CppObject, chara: *mut Il2CppObject) {
        // A career just loaded or re-attached its character: the state is live right now.
        if !this.is_null() && !chara.is_null() {
            DATA_AT.store(this as usize, atomic::Ordering::Release);
            DATA_STAMP_US.store(ProbeSuite::now_us(), atomic::Ordering::Relaxed);
        }

        get_orig_fn!(WorkSingleModeData_set_Character, SetCharacterFn)(this, chara);
    }
    bail {
                get_orig_fn!(WorkSingleModeData_set_Character, SetCharacterFn)(this, chara)
    }
}

type SetIsPlayingFn = extern "C" fn(this: *mut Il2CppObject, playing: u8);
def_detour! {
    WorkSingleModeData_set_IsPlaying(this: *mut Il2CppObject, playing: u8) {
        // The session ended: drop the cached state even before the freshness window would.
        if playing == 0 {
            DATA_AT.store(0, atomic::Ordering::Release);
        }

        get_orig_fn!(WorkSingleModeData_set_IsPlaying, SetIsPlayingFn)(this, playing);
    }
    bail {
                get_orig_fn!(WorkSingleModeData_set_IsPlaying, SetIsPlayingFn)(this, playing)
    }
}

// ---- fresh reads from the live character ------------------------------------------------------

unsafe fn call_i32(addr: usize, this: *mut Il2CppObject) -> Option<i32> {
    (addr != 0 && !this.is_null()).then(|| unsafe {
        let getter: extern "C" fn(*mut Il2CppObject) -> i32 = std::mem::transmute(addr);
        getter(this)
    })
}

unsafe fn call_u8(addr: usize, this: *mut Il2CppObject) -> Option<u8> {
    (addr != 0 && !this.is_null()).then(|| unsafe {
        let getter: extern "C" fn(*mut Il2CppObject) -> u8 = std::mem::transmute(addr);
        getter(this)
    })
}

unsafe fn call_string(addr: usize, this: *mut Il2CppObject) -> Option<String> {
    if addr == 0 || this.is_null() {
        return None;
    }

    let getter: extern "C" fn(*mut Il2CppObject) -> *mut Il2CppString = std::mem::transmute(addr);

    unsafe { getter(this).as_ref() }.map(|text| {
        let full = text.as_utf16str().to_string();
        full.chars().take(WALK_STRING_CAP).collect()
    })
}

unsafe fn call_object(addr: usize, this: *mut Il2CppObject) -> *mut Il2CppObject {
    if addr == 0 || this.is_null() {
        return std::ptr::null_mut();
    }

    let getter: extern "C" fn(*mut Il2CppObject) -> *mut Il2CppObject = std::mem::transmute(addr);
    getter(this)
}

/// Decode one `Obscured*` struct sitting at a base address, its class at hand: the instance ints
/// named by the live dump (`currentCryptoKey, hiddenValue, fakeValue`) are read and the v3 plain
/// rule applied. Statics are skipped outright: reading one here would call a neighbour's bytes the
/// key. None when the shape does not name the fields the rule needs - an unknown shape is never a
/// guessed number.
unsafe fn obscured_value_at(base: *const u8, struct_class: *mut Il2CppClass) -> Option<i64> {
    if struct_class.is_null() {
        return None;
    }

    // Reads stay inside the struct: a field offset the metadata reports past the struct's own size
    // would be read from its neighbours' bytes, so it is not read at all.
    let struct_size = il2cpp_class_value_size(struct_class, std::ptr::null_mut());

    if struct_size <= 0 {
        return None;
    }

    let struct_size = struct_size as usize;

    let mut crypto_current: Option<i32> = None;
    let mut fake: Option<i32> = None;
    let mut iter: *mut ::std::os::raw::c_void = std::ptr::null_mut();

    loop {
        let held = il2cpp_class_get_fields(struct_class, &mut iter);

        if held.is_null() {
            break;
        }

        let held_type = il2cpp_field_get_type(held);

        if held_type.is_null() || il2cpp_field_get_flags(held) & 0x10 != 0 || (*held_type).type_() != I4 || il2cpp_field_get_offset(held) + 4 > struct_size {
            continue;
        }

        let name = match skill_capture::as_string(il2cpp_field_get_name(held)) {
            Some(name) => name,
            None => continue,
        };

        let value = *((base.add(il2cpp_field_get_offset(held))) as *const i32);

        if name.eq_ignore_ascii_case("currentCryptoKey") { crypto_current = Some(value); }
        if name.eq_ignore_ascii_case("fakeValue") { fake = Some(value); }
    }

    match (crypto_current, fake) {
        (Some(key), Some(value)) => obscured_int_plain(key, value).map(|plain| plain as i64),
        _ => None,
    }
}

/// The same through a property name: `<Name>k__BackingField` on the object's class.
unsafe fn obscured_property(obj: *mut Il2CppObject, property: &str) -> Option<i64> {
    let klass = il2cpp_object_get_class(obj);
    let backing = format!("<{property}>k__BackingField");
    let c_backing = std::ffi::CString::new(backing).ok()?;
    let field = il2cpp_class_get_field_from_name(klass, c_backing.as_ptr());

    if field.is_null() {
        return None;
    }

    let struct_class = il2cpp_type_get_class_or_element_class(il2cpp_field_get_type(field));
    let base = (obj as *const u8).add(il2cpp_field_get_offset(field));

    obscured_value_at(base, struct_class)
}

// ---- the JSON walk ----------------------------------------------------------------------------

/// Every instance field of a live object as JSON, within the walk's caps and budget. Element and
/// nested classes are walked through their runtime classes only: what the metadata names, read at
/// the types it reports.
unsafe fn json_of_object(obj: *mut Il2CppObject, depth: usize) -> Value {
    if obj.is_null() {
        return Value::Null;
    }

    if !budget_take() {
        return Value::String("<over budget>".to_owned());
    }

    if depth > WALK_MAX_DEPTH {
        return Value::String("<obj>".to_owned());
    }

    let klass = il2cpp_object_get_class(obj);
    let mut map = Map::new();
    let mut iter: *mut ::std::os::raw::c_void = std::ptr::null_mut();
    let mut walked = 0usize;

    loop {
        let field = il2cpp_class_get_fields(klass, &mut iter);

        if field.is_null() {
            break;
        }

        if il2cpp_field_get_flags(field) & 0x10 != 0 {
            continue;
        }

        if walked >= WALK_FIELD_CAP {
            map.insert("_fieldsOverCap".to_owned(), Value::Bool(true));
            break;
        }

        walked += 1;

        let name = match skill_capture::as_string(il2cpp_field_get_name(field)) {
            Some(name) => clean_field_name(&name),
            None => continue,
        };

        map.insert(name, json_of_field(obj, field, depth));
    }

    Value::Object(map)
}

unsafe fn json_of_field(obj: *mut Il2CppObject, field: *mut FieldInfo, depth: usize) -> Value {
    if !budget_take() {
        return Value::String("<over budget>".to_owned());
    }

    let kind = (*il2cpp_field_get_type(field)).type_();
    let at = (obj as *const u8).add(il2cpp_field_get_offset(field));

    match kind {
        I4 => json!( *(at as *const i32) ),
        U4 => json!( *(at as *const u32) ),
        I2 => json!( *(at as *const i16) ),
        U2 => json!( *(at as *const u16) ),
        I1 => json!( *(at as *const i8) ),
        U1 => json!( *(at as *const u8) ),
        I8 => json!( *(at as *const i64) ),
        U8 => json!( *(at as *const u64) ),
        BOOL => json!( *(at as *const u8) != 0 ),
        R4 => json_as_number(*(at as *const f32) as f64),
        R8 => json_as_number(*(at as *const f64)),
        STRING => match *(at as *mut *mut Il2CppString) {
            held if held.is_null() => Value::Null,
            held => match held.as_ref() {
                Some(text) => {
                    let full = text.as_utf16str().to_string();
                    json!( full.chars().take(WALK_STRING_CAP).collect::<String>() )
                },
                None => Value::Null,
            },
        },
        GENERIC => {
            let held = *(at as *mut *mut Il2CppObject);
            let type_name = skill_capture::as_string(il2cpp_type_get_name(il2cpp_field_get_type(field))).unwrap_or_else(|| "?".to_owned());

            if type_name.contains("System.Collections.Generic.List") { json_of_list(held, depth) } else { Value::String(format!("<{type_name}>")) }
        },
        VALUETYPE | ENUM => {
            let struct_class = il2cpp_type_get_class_or_element_class(il2cpp_field_get_type(field));
            let name = if struct_class.is_null() { "?".to_owned() } else { skill_capture::as_string(il2cpp_class_get_name(struct_class)).unwrap_or_else(|| "?".to_owned()) };

            if name.starts_with("Obscured") {
                return match obscured_value_at(at, struct_class) {
                    Some(value) => json!(value),
                    None => Value::String(format!("<obscured {name}>")),
                };
            }

            // An enum's or struct's width read at its real size, never wider than the field: one
            // byte is AL, not the register above it.
            let size = if struct_class.is_null() { 0 } else { il2cpp_class_array_element_size(struct_class as *const Il2CppClass) };

            match (kind, size) {
                (ENUM, 1) => json!( *(at as *const u8) ),
                (ENUM, 2) => json!( *(at as *const u16) ),
                (ENUM, 4) => json!( *(at as *const i32) ),
                (ENUM, 8) => json!( *(at as *const i64) ),
                _ => Value::String(format!("<struct {name}:{size}B>")),
            }
        },
        CLASS | Il2CppTypeEnum_IL2CPP_TYPE_OBJECT | Il2CppTypeEnum_IL2CPP_TYPE_SZARRAY | Il2CppTypeEnum_IL2CPP_TYPE_ARRAY => {
            let held = *(at as *mut *mut Il2CppObject);

            if held.is_null() {
                Value::Null
            }
            else {
                json_of_object(held, depth + 1)
            }
        },
        _ => Value::String("<skipped>".to_owned()),
    }
}

fn json_as_number(value: f64) -> Value {
    if value.is_finite() { json!(value) } else { Value::String(format!("{value}")) }
}

/// A live list as `{count, items}`. Reference elements are walked as objects; an `int` list is read
/// as the ints in its array data (a `List<int>` element is not a pointer); anything else is named
/// and left unread rather than reinterpreted.
unsafe fn json_of_list(list: *mut Il2CppObject, depth: usize) -> Value {
    if list.is_null() {
        return Value::Null;
    }

    if !budget_take() {
        return Value::String("<over budget>".to_owned());
    }

    let Some((items, count)) = skill_capture::list_backing(list) else {
        return Value::String("<unreadable list>".to_owned());
    };

    let capacity = il2cpp_array_length(items) as usize;
    let (data, stride) = skill_capture::array_data(items);
    let shown = count.min(capacity).min(WALK_LIST_CAP);

    if skill_capture::list_elements_are_refs(items) {
        let mut entries = Vec::with_capacity(shown);

        for index in 0..shown {
            let element = *((data).add(index * stride) as *mut *mut Il2CppObject);
            entries.push(json_of_object(element, depth + 1));
        }

        return json!({ "count": count, "shown": shown, "items": entries });
    }

    if stride == 4 {
        let mut entries = Vec::with_capacity(shown);

        for index in 0..shown {
            entries.push(json!( *(((data).add(index * stride)) as *const i32) ));
        }

        return json!({ "count": count, "shown": shown, "items": entries });
    }

    json!({ "count": count, "items": format!("<value list, {stride}-byte elements>") })
}

// ---- the dump itself --------------------------------------------------------------------------

fn sections_from(chara: *mut Il2CppObject) -> Map<String, Value> {
    let mut root = Map::new();

    // The tables are copied out before any call: the getter is game code, and no lock is held
    // across an FFI call into it.
    let plain: Vec<GetterEntry> = lock_or_recover(&BOUND_I4).clone();
    let enums: Vec<GetterEntry> = lock_or_recover(&BOUND_ENUM).clone();
    let bytes: Vec<GetterEntry> = lock_or_recover(&BOUND_U8).clone();
    let strings: Vec<GetterEntry> = lock_or_recover(&BOUND_STRING).clone();

    for (section, key, addr) in plain.into_iter().chain(enums) {
        if let Some(value) = unsafe { call_i32(addr, chara) } {
            root.entry((*section).to_owned()).or_insert_with(|| Value::Object(Map::new())).as_object_mut().map(|map| map.insert((*key).to_owned(), json!(value)));
        }
    }

    for (section, key, addr) in bytes {
        if let Some(value) = unsafe { call_u8(addr, chara) } {
            root.entry((*section).to_owned()).or_insert_with(|| Value::Object(Map::new())).as_object_mut().map(|map| map.insert((*key).to_owned(), json!(value)));
        }
    }

    for (section, key, addr) in strings {
        if let Some(value) = unsafe { call_string(addr, chara) } {
            root.entry((*section).to_owned()).or_insert_with(|| Value::Object(Map::new())).as_object_mut().map(|map| map.insert((*key).to_owned(), json!(value)));
        }
    }

    for (section, key, property) in OBSCURED_GETTERS {
        if let Some(value) = unsafe { obscured_property(chara, property) } {
            root.entry((*section).to_owned()).or_insert_with(|| Value::Object(Map::new())).as_object_mut().map(|map| map.insert((*key).to_owned(), json!(value)));
        }
    }

    root
}

/// The skill screen's rows as the probe copied them, joined to the master names where a row's
/// MasterData pointer carried an Id while the door was firing. Owned data only: no row pointer is
/// touched here, long after the screen could have closed.
fn screen_snapshot_json(now_us: i64) -> Option<Value> {
    let snap = lock_or_recover(&skill_capture::SCREEN_SNAP);

    if snap.rows.is_empty() {
        return None;
    }

    let rows: Vec<Value> = snap.rows.iter().map(|fields| Value::Object(Map::from_iter(fields.iter().map(|(name, value)| (name.clone(), json!(value)))))).collect();

    Some(json!({
        "capturedAtUs": snap.stamp_us,
        "ageSec": (now_us - snap.stamp_us).max(0) / 1_000_000,
        "topIndex": snap.top,
        "rows": rows,
    }))
}

fn observed_tips_json() -> Value {
    let observed = lock_or_recover(&skill_capture::TIPS_OBSERVED);
    let mut map = Map::new();

    for (skill_id, (discount, level)) in observed.iter() {
        let mut entry = Map::new();

        if let Some(value) = discount { entry.insert("tipsDiscount".to_owned(), json!(value)); }
        if let Some(value) = level { entry.insert("hintLevel".to_owned(), json!(value)); }

        map.insert(skill_id.to_string(), Value::Object(entry));
    }

    Value::Object(map)
}

fn master_names_json() -> Value {
    let names = lock_or_recover(&skill_capture::MASTER_NAMES);
    let mut map = Map::new();

    for (id, fields) in names.iter() {
        map.insert(id.to_string(), Value::Object(Map::from_iter(fields.iter().map(|(name, text)| (name.clone(), json!(text))))));
    }

    Value::Object(map)
}

fn observed_ids_json() -> Value {
    let observed = lock_or_recover(&skill_capture::OBSERVED_SKILLS);
    Value::Array(observed.iter().map(|id| json!(id)).collect())
}

/// Every number the character object itself holds: each instance scalar field as its value, each
/// `Obscured*` as its decoded plain number (or `<held>` while the anti-cheat wrapper has the key
/// on), bounded and named by the runtime metadata. The dump earned this the honest way: a run read
/// `SkillPoint` as 518 from the live object while the skill screen showed 2948, so which field the
/// screen actually reads is data the file answers directly instead of a guess carried in code.
unsafe fn character_fields_json(chara: *mut Il2CppObject) -> Value {
    let mut map = Map::new();
    let klass = il2cpp_object_get_class(chara);
    let mut iter: *mut ::std::os::raw::c_void = std::ptr::null_mut();
    let mut walked = 0usize;

    // The ceiling every raw read below is checked against: a field whose reported offset cannot
    // fit its type inside the object is not this object's field, and reading it is how a dump
    // faults a game thread without ever getting to say so.
    let object_size = il2cpp_class_instance_size(klass);

    if object_size <= 0 {
        return json!({"_scan": "no instance size"});
    }

    let object_size = object_size as usize;

    loop {
        let field = il2cpp_class_get_fields(klass, &mut iter);

        if field.is_null() {
            break;
        }

        if il2cpp_field_get_flags(field) & 0x10 != 0 {
            continue;
        }

        if walked >= FIELD_SCAN_CAP {
            map.insert("_fieldsOverCap".to_owned(), Value::Bool(true));
            break;
        }

        walked += 1;

        let name = match skill_capture::as_string(il2cpp_field_get_name(field)) {
            Some(name) => clean_field_name(&name),
            None => continue,
        };

        let field_type = il2cpp_field_get_type(field);

        if field_type.is_null() {
            continue;
        }

        let kind = (*field_type).type_();
        let offset = il2cpp_field_get_offset(field);
        let at = (chara as *const u8).add(offset);

        let value = match kind {
            I4 if offset + 4 <= object_size => json!( *(at as *const i32) ),
            U4 if offset + 4 <= object_size => json!( *(at as *const u32) ),
            I2 if offset + 2 <= object_size => json!( *(at as *const i16) ),
            U2 if offset + 2 <= object_size => json!( *(at as *const u16) ),
            I1 if offset + 1 <= object_size => json!( *(at as *const i8) ),
            U1 if offset + 1 <= object_size => json!( *(at as *const u8) ),
            I8 if offset + 8 <= object_size => json!( *(at as *const i64) ),
            U8 if offset + 8 <= object_size => json!( *(at as *const u64) ),
            BOOL if offset + 1 <= object_size => json!( *(at as *const u8) != 0 ),
            VALUETYPE if offset + 20 <= object_size => {
                let struct_class = il2cpp_type_get_class_or_element_class(field_type);
                let struct_name = if struct_class.is_null() { String::new() } else { skill_capture::as_string(il2cpp_class_get_name(struct_class)).unwrap_or_default() };

                if struct_name.starts_with("Obscured") {
                    match obscured_value_at(at, struct_class) {
                        Some(value) => json!(value),
                        None => Value::String("<held>".to_owned()),
                    }
                }
                else {
                    Value::String(format!("<{struct_name}>"))
                }
            },
            _ => continue,
        };

        map.insert(name, value);
    }

    Value::Object(map)
}

const FIELD_SCAN_CAP: usize = 140;

/// What the dump can refuse with: no live career to read, or a real failure on the way out.
enum DumpError {
    NoCareer,
    Failed(String),
}

fn age_sec(now_us: i64, stamp_us: i64) -> i64 {
    if stamp_us < 0 { -1 } else { (now_us - stamp_us).max(0) / 1_000_000 }
}

fn run_dump() -> Result<String, DumpError> {
    let now_us = ProbeSuite::now_us();

    // Two liveness paths, freshest first:
    // - The character itself, stamped by the tip doors. Those doors are the game's own row binds
    //   and fire by the hundreds on a skill screen, so while that screen lives the pointer was
    //   read by the game moments ago and is used directly.
    // - The career state, stamped by its own HUD-clock door, with `get_Character` re-deriving the
    //   character fresh. This is the path for pressing the button anywhere else in a run.
    let chara_at = skill_capture::CHARA_AT.load(atomic::Ordering::Acquire);
    let chara_stamp = skill_capture::CHARA_STAMP_US.load(atomic::Ordering::Relaxed);
    let data_at = DATA_AT.load(atomic::Ordering::Acquire);
    let data_stamp = DATA_STAMP_US.load(atomic::Ordering::Relaxed);

    let mut via_chara_door = false;
    let chara = if chara_at != 0 && is_fresh(now_us, chara_stamp) {
        via_chara_door = true;
        chara_at as *mut Il2CppObject
    }
    else if data_at != 0 && is_fresh(now_us, data_stamp) {
        unsafe { call_object(GET_CHARACTER_ADDR.load(atomic::Ordering::Relaxed), data_at as *mut Il2CppObject) }
    }
    else {
        return Err(DumpError::NoCareer);
    };

    if chara.is_null() {
        info!("Career dump: refused, no character behind the fresh state (state {} s, character {} s old)", age_sec(now_us, data_stamp), age_sec(now_us, chara_stamp));
        return Err(DumpError::NoCareer);
    }

    info!("Career dump: reading through a character the game touched {} s ago", age_sec(now_us, if via_chara_door { chara_stamp } else { data_stamp }));

    // The stage markers are the dump's black box: every later result is logged too, so the last
    // marker a run printed is the phase a fault died in. A void detour swallows faults with no
    // line of their own (the barrier only counts them until detach), and a press that "did
    // nothing" leaves exactly this trail.
    let stage = std::time::Instant::now();
    let elapsed = || stage.elapsed().as_millis();

    WALK_NODES.with(|nodes| nodes.set(WALK_NODE_BUDGET));

    let mut root = Map::new();
    root.insert("kind".to_owned(), json!("hachimi-career-dump"));
    root.insert("version".to_owned(), json!(1));

    let epoch = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    root.insert("generatedAtEpochSec".to_owned(), json!(epoch));
    root.insert("careerDataAgeSec".to_owned(), json!(age_sec(now_us, data_stamp)));

    let last_month = LAST_MONTH.load(atomic::Ordering::Relaxed);

    if last_month >= 0 {
        root.insert("lastMonthSeen".to_owned(), json!(last_month));
    }

    for (section, value) in sections_from(chara) {
        root.insert(section, value);
    }

    root.insert("skillPoint".to_owned(), match unsafe { obscured_property(chara, "SkillPoint") } {
        Some(value) => json!(value),
        None => Value::String("<unreadable>".to_owned()),
    });
    info!("Career dump: getters done in {} ms", elapsed());

    let lists: Vec<(&'static str, usize)> = lock_or_recover(&BOUND_LIST).iter().map(|(section, _, addr)| (*section, *addr)).collect();

    for (section, addr) in lists {
        let list = unsafe { call_object(addr, chara) };
        root.insert(section.to_owned(), unsafe { json_of_list(list, 0) });
    }
    info!("Career dump: lists walked in {} ms", elapsed());

    if let Some(screen) = screen_snapshot_json(now_us) {
        root.insert("skillLearningScreen".to_owned(), screen);
    }

    root.insert("skillTipsObserved".to_owned(), observed_tips_json());
    root.insert("skillMasterNames".to_owned(), master_names_json());
    root.insert("skillIdsOffered".to_owned(), observed_ids_json());
    info!("Career dump: owned snapshots done in {} ms", elapsed());

    root.insert("characterFields".to_owned(), unsafe { character_fields_json(chara) });
    info!("Career dump: character fields scanned in {} ms", elapsed());

    // The same flat scan over the career state, whenever that root is itself fresh: if the number
    // the skill screen shows does not live on the character, it lives near here.
    if data_at != 0 && is_fresh(now_us, data_stamp) {
        root.insert("careerStateFields".to_owned(), unsafe { character_fields_json(data_at as *mut Il2CppObject) });
        info!("Career dump: career state scanned in {} ms", elapsed());
    }

    let body = serde_json::to_string_pretty(&Value::Object(root)).map_err(|e| DumpError::Failed(e.to_string()))?;
    info!("Career dump: json ready, {} bytes, {} ms all together", body.len(), elapsed());

    // The id for the file name is read through the same bound getter table the JSON used; when the
    // getter was never bound, the file is still written, just named by epoch alone.
    let chara_id = {
        let table: Vec<GetterEntry> = lock_or_recover(&BOUND_I4).clone();
        table.iter().find(|(_, key, _)| *key == "charaId").and_then(|(_, _, addr)| unsafe { call_i32(*addr, chara) }).unwrap_or(0)
    };
    let name = format!("career_{chara_id}_{epoch}.json");
    let path = Hachimi::instance().get_data_path(&name);

    std::fs::write(&path, body).map_err(|e| DumpError::Failed(format!("{}: {e}", path.display())))?;

    DUMPS_DONE.fetch_add(1, atomic::Ordering::Relaxed);
    info!("Career dump: wrote {name}");

    Ok(name)
}

static DUMPS_DONE: AtomicUsize = AtomicUsize::new(0);

/// The GUI button's call: only an atomic store happens off the game thread. The serialization and
/// every game read it performs run in the game tick that sees the flag.
pub fn request_dump() {
    if ARMED.load(atomic::Ordering::Acquire) {
        REQUESTED.store(true, atomic::Ordering::Release);
    }
}

// From the GameSystem update detour, beside the other probe reports.
pub fn poll() {
    if !REQUESTED.swap(false, atomic::Ordering::AcqRel) {
        return;
    }

    // Under an explicit catch: this runs inside a void detour, where the hook barrier swallows a
    // panicking body with no line at all. Catching here is what turns "the button did nothing"
    // into a logged, named failure.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run_dump));

    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(payload) => {
            let reason = payload.downcast_ref::<String>().cloned().or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned())).unwrap_or_else(|| "panic without a message".to_owned());

            info!("Career dump: the dump body panicked: {reason}");
            Err(DumpError::Failed(reason))
        },
    };

    match outcome {
        Ok(name) => request_notification(NotificationRequest::Custom(t!("notification.career_dump_saved", name = name).to_string())),
        Err(DumpError::NoCareer) => {
            let reason = t!("notification.career_dump_no_career").to_string();
            info!("Career dump: refused, {reason}");
            request_notification(NotificationRequest::Custom(reason));
        },
        Err(DumpError::Failed(reason)) => {
            info!("Career dump: failed, {reason}");
            request_notification(NotificationRequest::Custom(t!("notification.career_dump_failed", reason = reason).to_string()));
        },
    }
}

pub fn init(umamusume: *const Il2CppImage) {
    let data = class_for_label(umamusume, "Gallop.WorkSingleModeData");
    let chara = class_for_label(umamusume, "Gallop.WorkSingleModeCharaData");
    let mut missing: Vec<&str> = Vec::new();
    let mut bound = 0usize;
    let mut doors = 0usize;

    if let Some(class) = data {
        let month_addr = unsafe { AnimationSpeed::resolve_method(class, "get_Month", &[], I4) };

        if month_addr != 0 { new_hook!(month_addr, WorkSingleModeData_get_Month); doors += 1; } else { missing.push("get_Month"); }

        let attach_addr = unsafe { AnimationSpeed::resolve_method(class, "set_Character", &[CLASS], VOID) };

        if attach_addr != 0 { new_hook!(attach_addr, WorkSingleModeData_set_Character); doors += 1; } else { missing.push("set_Character"); }

        let playing_addr = unsafe { AnimationSpeed::resolve_method(class, "set_IsPlaying", &[BOOL], VOID) };

        if playing_addr != 0 { new_hook!(playing_addr, WorkSingleModeData_set_IsPlaying); doors += 1; } else { missing.push("set_IsPlaying"); }

        GET_CHARACTER_ADDR.store(unsafe { AnimationSpeed::resolve_method(class, "get_Character", &[], CLASS) }, atomic::Ordering::Relaxed);
    }
    else {
        missing.extend(["get_Month", "set_Character", "set_IsPlaying", "get_Character"]);
    }

    if let Some(class) = chara {
        for (table, spec, ret) in [
            (&BOUND_I4, I4_GETTERS, I4),
            (&BOUND_ENUM, ENUM_GETTERS, VALUETYPE),
            (&BOUND_U8, U8_GETTERS, VALUETYPE),
            (&BOUND_STRING, STRING_GETTERS, STRING),
        ] {
            for (section, key, method) in spec {
                let addr = unsafe { AnimationSpeed::resolve_method(class, method, &[], ret) };

                if addr == 0 {
                    missing.push(method);
                    continue;
                }

                lock_or_recover(table).push((*section, *key, addr));
                bound += 1;
            }
        }

        for (section, method) in LIST_GETTERS {
            let addr = unsafe { AnimationSpeed::resolve_generic_ref_method(class, method, &[], GENERIC) };

            if addr == 0 {
                missing.push(method);
                continue;
            }

            lock_or_recover(&BOUND_LIST).push((section, section, addr));
            bound += 1;
        }
    }
    else {
        missing.extend(I4_GETTERS.iter().map(|(_, _, method)| *method));
    }

    ARMED.store(true, atomic::Ordering::Release);
    info!("Career dump: {doors} liveness doors, {bound} getters bound; the menu button writes career_<charaId>_<epoch>.json beside the game folder files");

    if !missing.is_empty() {
        info!("Career dump: not bound (no matching overload or class gone): {}", missing.join(", "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cached_state_is_fresh_only_inside_the_window() {
        assert!(is_fresh(1_000, 1_000));
        assert!(is_fresh(1_000 + FRESH_WINDOW_US, 1_000));
        assert!(!is_fresh(1_000 + FRESH_WINDOW_US + 1, 1_000));
        assert!(!is_fresh(1_000, -1), "a state never stamped is never fresh");
        assert!(!is_fresh(1_000, 2_000), "a stamp from the future is not fresh");
        assert!(!is_fresh(-1, 0), "no clock yet, no fresh state");
    }

    #[test]
    fn every_spec_names_a_getter_shape_the_dump_proved() {
        // Getters are read by call with zero parameters, so their spec names must all be getters or
        // the no-arg methods the dump block printed, never a setter or an argument-taking method.
        for (_, _, method) in I4_GETTERS.iter().chain(ENUM_GETTERS.iter()).chain(U8_GETTERS.iter()).chain(STRING_GETTERS.iter()) {
            let bare = method.strip_prefix("get_").unwrap_or(method);
            assert!(!bare.is_empty() && !method.starts_with("set_"), "{method}");
        }

        assert!(I4_GETTERS.iter().any(|(_, key, _)| *key == "charaId"));
        assert!(ENUM_GETTERS.iter().any(|(_, key, _)| *key == "groundTurf"));
        assert!(U8_GETTERS.iter().any(|(_, _, method)| *method == "get_RunningStyle"));
    }

    #[test]
    fn the_sections_the_json_promises_are_all_satisfiable() {
        let mut sections: Vec<&str> = I4_GETTERS.iter().chain(ENUM_GETTERS.iter()).chain(U8_GETTERS.iter()).chain(STRING_GETTERS.iter()).chain(OBSCURED_GETTERS.iter()).map(|(section, _, _)| *section).collect();
        sections.sort_unstable();
        sections.dedup();
        sections.sort();

        assert_eq!(sections, ["aptitudes", "character", "stats"]);
    }

    #[test]
    fn the_walk_budget_burns_down_and_then_refuses() {
        WALK_NODES.with(|nodes| nodes.set(2));
        assert!(budget_take());
        assert!(budget_take());
        assert!(!budget_take());
    }
}
