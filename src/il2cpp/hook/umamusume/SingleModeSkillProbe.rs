//! Observe-only probe on the career skill-buy screen: what the screen offers, what it costs, and what
//! the character can pay.
//!
//! The question this answers is what the player asked: capture the skills a career can buy right now,
//! with name, price, hint discount, and the current SP, as data, from a normal play session. The
//! authoritative moment for all four is the skill-learning screen building its rows, and every door
//! below is a signature the live dump already printed, so nothing here is guessed (C47).
//!
//! The four sources are:
//! - `SingleModeUtils::CreateSkillInfoBySkillIdList/1 -> static generic<List<SkillInfo>>(generic<List<int>>)`,
//!   `CalcNeedRareSkillPoint/1 -> static void(generic<List<SkillInfo>>)`, and
//!   `GetTopIndexFromSkillLerningListItemInfoList/1 -> static int(generic<List<Info>>)`
//!   (`introspect-run69.log`, the `Gallop.SingleModeUtils` block): the screen's own lists. `SkillInfo` and
//!   `Info` are nested types no metadata dump has ever printed, so they are not guessed at either: each
//!   element is walked through its *runtime* class, every field named and read by its real type. What the
//!   log shows is the whole row, not a subset picked in advance.
//! - `SingleModeUtils::GetSkillFrameSprite/1 -> static class<Sprite>(class<MasterSkillData.SkillData>)`
//!   (same block): the master-data row the UI renders per skill. Its string fields are the skill's name.
//! - `Gallop.WorkSingleModeCharaData::GetTipsSkillPointDiscount/1 -> int(int)`,
//!   `GetTipsSkillLevel/1 -> int(int)`, `CalcNeedSkillPoint/2 -> int(int, int)`
//!   (`introspect-run69.log`, the `Gallop.WorkSingleModeCharaData` block): the per-skill discount, the hint
//!   level behind it, and the game's own price computation. The class is the career character: its
//!   `ApplySkillUpgrade` takes `DialogSingleModeSkillUpgradeSelect.SelectResult`.
//!
//! The SP is read off the same `this` the discount doors receive, at the offset of
//! `field <SkillPoint>k__BackingField [struct<ObscuredInt:20B>]` from that block, so no struct return
//! ever travels through a wrapper (A5). `ObscuredInt` is the anti-cheat int wrapper; its own fields are
//! read by their runtime names, the raw ints are always logged, and the decoded value is labelled as a
//! decode so a run can check it against the number on screen.
//!
//! Nothing here changes an argument or a return value. Every wrapper hands the call to the game
//! untouched, and the whole module is behind `debug_mode`, so a player who has that off gets no hook at all.

use std::fmt::Write as _;
use std::os::raw::c_void;
use std::sync::atomic::{self, AtomicBool, AtomicI64, AtomicUsize};
use std::sync::Mutex;

use crate::{
    core::Hachimi,
    il2cpp::{
        api::*,
        ext::Il2CppStringExt,
        hook::umamusume::{
            AnimationSpeed,
            ProbeSuite,
            TrainingCuttProbe::{class_for_label, report_due, CutProbe, PROBE_DETAIL_LIMIT, REPORT_INTERVAL_SECS},
        },
        types::*,
    },
};

const I4: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_I4;
const R4: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_R4;
const BOOL: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN;
const CLASS: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_CLASS;
const VOID: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_VOID;
const GENERIC: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_GENERICINST;
const STRING: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_STRING;
const VALUETYPE: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE;
const ENUM: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_ENUM;
const ONE_GENERIC: &[Il2CppTypeEnum] = &[GENERIC];
const ONE_INT: &[Il2CppTypeEnum] = &[I4];
const TWO_INTS: &[Il2CppTypeEnum] = &[I4, I4];
const ONE_MASTER_ROW: &[Il2CppTypeEnum] = &[CLASS];

static CREATE_SKILL_INFO: CutProbe = CutProbe::traced("SingleModeUtils::CreateSkillInfoBySkillIdList(ids)", PROBE_DETAIL_LIMIT);
static CALC_RARE: CutProbe = CutProbe::traced("SingleModeUtils::CalcNeedRareSkillPoint(skills)", PROBE_DETAIL_LIMIT);
static TOP_INDEX: CutProbe = CutProbe::traced("SingleModeUtils::GetTopIndexFromSkillLerningListItemInfoList(rows)", PROBE_DETAIL_LIMIT);
static MASTER_ROW: CutProbe = CutProbe::traced("SingleModeUtils::GetSkillFrameSprite(masterRow)", 24);
static TIPS_DISCOUNT: CutProbe = CutProbe::traced("WorkSingleModeCharaData::GetTipsSkillPointDiscount(skillId)", 40);
static TIPS_LEVEL: CutProbe = CutProbe::traced("WorkSingleModeCharaData::GetTipsSkillLevel(skillId)", 40);
static NEED_POINT: CutProbe = CutProbe::traced("WorkSingleModeCharaData::CalcNeedSkillPoint(a, b)", 40);

pub(crate) static PROBES: [&CutProbe; 7] = [
    &CREATE_SKILL_INFO,
    &CALC_RARE,
    &TOP_INDEX,
    &MASTER_ROW,
    &TIPS_DISCOUNT,
    &TIPS_LEVEL,
    &NEED_POINT,
];

static ARMED: AtomicBool = AtomicBool::new(false);
static LAST_REPORT_SEC: AtomicI64 = AtomicI64::new(-1);
static LAST_TOTALS: AtomicUsize = AtomicUsize::new(0);

// The SP read: offset of `field <SkillPoint>k__BackingField` on the character data, plus the int fields
// of the ObscuredInt struct it holds, by runtime name. Empty when the field name moved: the doors then
// report every other number and simply say the SP field is not there.
static SP_FIELD_OFFSET: AtomicUsize = AtomicUsize::new(0);
static SP_SHAPE: Mutex<Vec<(String, usize)>> = Mutex::new(Vec::new());

// How many rows the element dumps print in full, and how deep one row's own nested list goes. A row that
// outgrows these is counted and named, never silently half-printed.
const FIELD_CAP: usize = 24;
const ROW_CAP: usize = 14;
const STRING_CAP: usize = 64;

// The `ObscuredInt` state value that means "the int is masked" in the anti-cheat wrapper: the four bytes
// of "ALLD", its documented magic. Anything else and the value field is the plain number.
const OBSCURED_MASKED: i32 = 0x4C4C_4441;

/// The decode candidate for an `ObscuredInt` read as `(value field, mask field, state field)` ints.
/// The masked state means value and mask are the two halves of one number; any other state says the
/// value is already plain. A read without a value field decodes to nothing rather than to a guess.
pub(crate) fn obscured_int_decode(value: i32, mask: i32, state: Option<i32>) -> i32 {
    match state {
        Some(OBSCURED_MASKED) => value ^ mask,
        _ => value,
    }
}

fn sp_text(chara: *mut Il2CppObject) -> String {
    let offset = SP_FIELD_OFFSET.load(atomic::Ordering::Relaxed);

    if offset == 0 || chara.is_null() {
        return String::from(" SP field not read");
    }

    let base = unsafe { (chara as *mut u8).add(offset) };
    let shape = SP_SHAPE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let mut out = String::from(" SP raw[");
    let mut value: Option<i32> = None;
    let mut mask: Option<i32> = None;
    let mut state: Option<i32> = None;

    for (name, field_offset) in &shape {
        let held = unsafe { *(base.add(*field_offset) as *const i32) };
        let _ = write!(out, " {name}={held}");

        if name.contains("value") { value = Some(held); }
        if name.contains("mask") { mask = Some(held); }
        if name.contains("state") { state = Some(held); }
    }

    out.push(']');

    if let (Some(v), Some(m)) = (value, mask) {
        let _ = write!(out, " decoded {}", obscured_int_decode(v, m, state));
    }

    out
}

/// `name=value` for one instance field of a live object. Every primitive the metadata can name is read;
/// strings are printed truncated; a generic `List<T>` is printed by its live size; anything else is
/// named by type so the shape is still visible. A null object or field prints its own marker instead of
/// reading through it.
fn field_value_text(obj: *mut Il2CppObject, field: *mut FieldInfo) -> String {
    let kind = unsafe { (*il2cpp_field_get_type(field)).type_() };
    let at = unsafe { (obj as *mut u8).add(il2cpp_field_get_offset(field)) };

    match kind {
        I4 => format!("{}", unsafe { *(at as *const i32) }),
        Il2CppTypeEnum_IL2CPP_TYPE_U4 => format!("{}", unsafe { *(at as *const u32) }),
        Il2CppTypeEnum_IL2CPP_TYPE_I2 => format!("{}", unsafe { *(at as *const i16) }),
        Il2CppTypeEnum_IL2CPP_TYPE_U2 => format!("{}", unsafe { *(at as *const u16) }),
        Il2CppTypeEnum_IL2CPP_TYPE_I1 => format!("{}", unsafe { *(at as *const i8) }),
        Il2CppTypeEnum_IL2CPP_TYPE_U1 => format!("{}", unsafe { *(at as *const u8) }),
        Il2CppTypeEnum_IL2CPP_TYPE_I8 => format!("{}", unsafe { *(at as *const i64) }),
        Il2CppTypeEnum_IL2CPP_TYPE_U8 => format!("{}", unsafe { *(at as *const u64) }),
        BOOL => format!("{}", unsafe { *(at as *const u8) } != 0),
        R4 => format!("{:.3}", unsafe { *(at as *const f32) }),
        Il2CppTypeEnum_IL2CPP_TYPE_R8 => format!("{}", unsafe { *(at as *const f64) }),
        STRING => {
            let held = unsafe { *(at as *mut *mut Il2CppString) };

            match unsafe { held.as_ref() } {
                Some(text) => {
                    let full = text.as_utf16str().to_string();
                    let short: String = full.chars().take(STRING_CAP).collect();

                    if short.chars().count() < full.chars().count() { format!("\"{short}..") } else { format!("\"{short}\"") }
                },
                None => String::from("<null>"),
            }
        },
        GENERIC => {
            let held = unsafe { *(at as *mut *mut Il2CppObject) };
            let type_name = as_string(il2cpp_type_get_name(il2cpp_field_get_type(field))).unwrap_or_else(|| "?".to_owned());

            if !type_name.contains("System.Collections.Generic.List") || held.is_null() {
                return format!("<{type_name}>");
            }

            format!("List{} ", list_text(held))
        },
        VALUETYPE | ENUM => {
            let class = il2cpp_type_get_class_or_element_class(il2cpp_field_get_type(field));
            let name = if class.is_null() { "?".to_owned() } else { as_string(il2cpp_class_get_name(class)).unwrap_or_else(|| "?".to_owned()) };

            format!("<struct {name}>")
        },
        CLASS | Il2CppTypeEnum_IL2CPP_TYPE_OBJECT | Il2CppTypeEnum_IL2CPP_TYPE_SZARRAY | Il2CppTypeEnum_IL2CPP_TYPE_ARRAY => {
            let held = unsafe { *(at as *mut *mut c_void) };

            if held.is_null() { String::from("<null>") } else { String::from("<obj>") }
        },
        _ => String::from("<skipped>"),
    }
}

fn as_string(ptr: *const ::std::os::raw::c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }

    unsafe { std::ffi::CStr::from_ptr(ptr) }.to_str().ok().map(str::to_owned)
}

/// The live size of a `List<T>`, read through its own `_size` field. A list whose shape this client
/// spells differently reports as unreadable rather than as empty.
fn list_size(list: *mut Il2CppObject) -> Option<usize> {
    let klass = il2cpp_object_get_class(list);
    let size_field = il2cpp_class_get_field_from_name(klass, c"_size".as_ptr());
    let items_field = il2cpp_class_get_field_from_name(klass, c"_items".as_ptr());

    if size_field.is_null() || items_field.is_null() {
        return None;
    }

    let count = unsafe { *( (list as *mut u8).add(il2cpp_field_get_offset(size_field)) as *const i32 ) };

    if count < 0 {
        return None;
    }

    Some(count as usize)
}

/// `n items [first element preview]` for a live list. Only the first element is previewed inside a
/// field dump; row dumps walk the list itself.
fn list_text(list: *mut Il2CppObject) -> String {
    match list_size(list) {
        Some(count) => format!("[{count} items]"),
        None => String::from("[unreadable list]"),
    }
}

/// Walk a live `List<int>`. The ints live in the array data itself, not behind element pointers, so
/// reading them as pointers would dereference a small number as an address.
fn dump_int_list(out: &mut String, list: *mut Il2CppObject) {
    if list.is_null() {
        out.push_str("<null list>");
        return;
    }

    let Some(count) = list_size(list) else {
        out.push_str("<unreadable list>");
        return;
    };

    let _ = write!(out, "[{count} ids");

    let klass = il2cpp_object_get_class(list);
    let items_field = il2cpp_class_get_field_from_name(klass, c"_items".as_ptr());

    if items_field.is_null() {
        out.push(']');
        return;
    }

    let items = unsafe { *( (list as *mut u8).add(il2cpp_field_get_offset(items_field)) as *mut *mut Il2CppArray ) };

    if items.is_null() {
        out.push(']');
        return;
    }

    let capacity = il2cpp_array_length(items) as usize;
    let stride = il2cpp_array_element_size(il2cpp_object_get_class(items as *mut Il2CppObject)) as usize;
    let data = unsafe { (items as *mut u8).add(il2cpp_array_object_header_size() as usize) };
    let shown = count.min(capacity).min(ROW_CAP);

    for index in 0..shown {
        let held = unsafe { *(((data).add(index * stride)) as *const i32) };
        let _ = write!(out, " {held}");
    }

    if count > shown {
        let _ = write!(out, " +{} over the cap", count - shown);
    }

    out.push(']');
}

/// Walk a live `List<T>` of references and print every element's fields, capped at `ROW_CAP`.
/// The element pointer is read from the array's data area at the stride the runtime itself reports.
fn dump_ref_list(out: &mut String, list: *mut Il2CppObject) {
    if list.is_null() {
        out.push_str("<null list>");
        return;
    }

    let Some(count) = list_size(list) else {
        out.push_str("<unreadable list>");
        return;
    };

    let _ = write!(out, "[{count} rows]");

    let klass = il2cpp_object_get_class(list);
    let items_field = il2cpp_class_get_field_from_name(klass, c"_items".as_ptr());

    if items_field.is_null() {
        return;
    }

    let items = unsafe { *( (list as *mut u8).add(il2cpp_field_get_offset(items_field)) as *mut *mut Il2CppArray ) };

    if items.is_null() {
        return;
    }

    let capacity = il2cpp_array_length(items) as usize;
    let stride = il2cpp_array_element_size(il2cpp_object_get_class(items as *mut Il2CppObject)) as usize;
    let data = unsafe { (items as *mut u8).add(il2cpp_array_object_header_size() as usize) };
    let shown = count.min(capacity).min(ROW_CAP);

    for index in 0..shown {
        let element = unsafe { *((data).add(index * stride) as *mut *mut Il2CppObject) };

        out.push_str(" row{index}:");
        dump_object_fields(out, element);
    }

    if count > shown {
        let _ = write!(out, " +{} rows over the cap", count - shown);
    }
}

/// Every instance field of one live object, by runtime metadata name. Static fields are skipped: the
/// values that matter are the ones this object holds.
fn dump_object_fields(out: &mut String, obj: *mut Il2CppObject) {
    if obj.is_null() {
        out.push_str("<null>");
        return;
    }

    let klass = il2cpp_object_get_class(obj);
    let mut iter: *mut ::std::os::raw::c_void = std::ptr::null_mut();
    let mut printed = 0usize;
    let mut total = 0usize;

    out.push('{');

    loop {
        let field = il2cpp_class_get_fields(klass, &mut iter);

        if field.is_null() {
            break;
        }

        // Static fields have no slot inside this object; the dump reads what the instance holds.
        if il2cpp_field_get_flags(field) & 0x10 != 0 {
            continue;
        }

        total += 1;

        if printed >= FIELD_CAP {
            continue;
        }

        printed += 1;

        let name = as_string(il2cpp_field_get_name(field)).unwrap_or_else(|| "?".to_owned());
        let _ = write!(out, " {name}={}", field_value_text(obj, field));
    }

    out.push('}');

    if total > printed {
        let _ = write!(out, " +{} fields over the cap", total - printed);
    }
}

/// A counted door whose lines stop at a caller-named limit. Formatting runs only while lines are still
/// owed, so a screen re-bound every purchase stays quiet after the first rows.
fn detail_line(door: &CutProbe, limit: usize, text: impl FnOnce() -> String) {
    door.count();

    if door.calls() <= limit {
        info!("Skill buy probe: {}", text());
    }
}

type CreateSkillInfoFn = extern "C" fn(ids: *mut Il2CppObject) -> *mut Il2CppObject;
def_detour! {
    SingleModeUtils_CreateSkillInfoBySkillIdList(ids: *mut Il2CppObject) -> *mut Il2CppObject {
            CREATE_SKILL_INFO.count();

        let result = get_orig_fn!(SingleModeUtils_CreateSkillInfoBySkillIdList, CreateSkillInfoFn)(ids);

        if CREATE_SKILL_INFO.calls() <= PROBE_DETAIL_LIMIT {
            let mut line = String::from("built from ids ");
            dump_int_list(&mut line, ids);
            line.push_str(" -> skills ");
            dump_ref_list(&mut line, result);
            info!("Skill buy probe: {line}");
        }

        result
    }
    bail {
                get_orig_fn!(SingleModeUtils_CreateSkillInfoBySkillIdList, CreateSkillInfoFn)(ids)
    }
}

type CalcRareFn = extern "C" fn(skills: *mut Il2CppObject);
def_detour! {
    SingleModeUtils_CalcNeedRareSkillPoint(skills: *mut Il2CppObject) {
            CALC_RARE.count();

        get_orig_fn!(SingleModeUtils_CalcNeedRareSkillPoint, CalcRareFn)(skills);

        // After the game's own pass: the same list carries the rare-point totals it just computed.
        if CALC_RARE.calls() <= PROBE_DETAIL_LIMIT {
            let mut line = String::from("costs ");
            dump_ref_list(&mut line, skills);
            info!("Skill buy probe: {line}");
        }
    }
    bail {
                get_orig_fn!(SingleModeUtils_CalcNeedRareSkillPoint, CalcRareFn)(skills)
    }
}

type TopIndexFn = extern "C" fn(rows: *mut Il2CppObject) -> i32;
def_detour! {
    SingleModeUtils_GetTopIndexFromSkillLerningListItemInfoList(rows: *mut Il2CppObject) -> i32 {
            TOP_INDEX.count();

        let top = get_orig_fn!(SingleModeUtils_GetTopIndexFromSkillLerningListItemInfoList, TopIndexFn)(rows);

        if TOP_INDEX.calls() <= PROBE_DETAIL_LIMIT {
            let mut line = format!("top {top} rows ");
            dump_ref_list(&mut line, rows);
            info!("Skill buy probe: {line}");
        }

        top
    }
    bail {
                get_orig_fn!(SingleModeUtils_GetTopIndexFromSkillLerningListItemInfoList, TopIndexFn)(rows)
    }
}

type MasterRowFn = extern "C" fn(row: *mut Il2CppObject) -> *mut Il2CppObject;
def_detour! {
    SingleModeUtils_GetSkillFrameSprite(row: *mut Il2CppObject) -> *mut Il2CppObject {
            MASTER_ROW.count();

        // The master row is the skill's name and its catalogue entry. Rows repeat every cell bind, so
        // only the first ones print; the count carries the rest.
        if MASTER_ROW.calls() <= 24 {
            let mut line = format!("master call {}: ", MASTER_ROW.calls());
            dump_object_fields(&mut line, row);
            info!("Skill buy probe: {line}");
        }

        get_orig_fn!(SingleModeUtils_GetSkillFrameSprite, MasterRowFn)(row)
    }
    bail {
                get_orig_fn!(SingleModeUtils_GetSkillFrameSprite, MasterRowFn)(row)
    }
}

type DiscountFn = extern "C" fn(this: *mut Il2CppObject, skill_id: i32) -> i32;
def_detour! {
    WorkSingleModeCharaData_GetTipsSkillPointDiscount(this: *mut Il2CppObject, skill_id: i32) -> i32 {
        let result = get_orig_fn!(WorkSingleModeCharaData_GetTipsSkillPointDiscount, DiscountFn)(this, skill_id);

        // The first reads carry the SP so one line per row answers cost and affordability at once;
        // later ones stay short because the number does not move inside one screen.
        detail_line(&TIPS_DISCOUNT, 40, || {
            let extra = if TIPS_DISCOUNT.calls() <= PROBE_DETAIL_LIMIT { sp_text(this) } else { String::new() };
            format!("skill {skill_id} tips discount {result}{extra}")
        });

        result
    }
    bail {
                get_orig_fn!(WorkSingleModeCharaData_GetTipsSkillPointDiscount, DiscountFn)(this, skill_id)
    }
}

type LevelFn = extern "C" fn(this: *mut Il2CppObject, skill_id: i32) -> i32;
def_detour! {
    WorkSingleModeCharaData_GetTipsSkillLevel(this: *mut Il2CppObject, skill_id: i32) -> i32 {
        let result = get_orig_fn!(WorkSingleModeCharaData_GetTipsSkillLevel, LevelFn)(this, skill_id);

        detail_line(&TIPS_LEVEL, 40, || format!("skill {skill_id} hint level {result}"));

        result
    }
    bail {
                get_orig_fn!(WorkSingleModeCharaData_GetTipsSkillLevel, LevelFn)(this, skill_id)
    }
}

type NeedPointFn = extern "C" fn(this: *mut Il2CppObject, first: i32, second: i32) -> i32;
def_detour! {
    WorkSingleModeCharaData_CalcNeedSkillPoint(this: *mut Il2CppObject, first: i32, second: i32) -> i32 {
        let result = get_orig_fn!(WorkSingleModeCharaData_CalcNeedSkillPoint, NeedPointFn)(this, first, second);

        detail_line(&NEED_POINT, 40, || format!("need skill point ({first}, {second}) -> {result}"));

        result
    }
    bail {
                get_orig_fn!(WorkSingleModeCharaData_CalcNeedSkillPoint, NeedPointFn)(this, first, second)
    }
}

/// The SP slot and the int shape of the struct that holds it, read once from live metadata. A name the
/// client lacks leaves the shape empty and every door says so, which beats reading unknown bytes.
fn read_sp_shape(chara: *mut Il2CppClass) {
    let field = il2cpp_class_get_field_from_name(chara, c"<SkillPoint>k__BackingField".as_ptr());

    if field.is_null() {
        info!("Skill buy probe: no <SkillPoint>k__BackingField on WorkSingleModeCharaData, SP will not be read");
        return;
    }

    SP_FIELD_OFFSET.store(il2cpp_field_get_offset(field), atomic::Ordering::Relaxed);

    let struct_class = il2cpp_type_get_class_or_element_class(il2cpp_field_get_type(field));

    if struct_class.is_null() {
        return;
    }

    let mut shape = Vec::new();
    let mut iter: *mut ::std::os::raw::c_void = std::ptr::null_mut();

    loop {
        let held = il2cpp_class_get_fields(struct_class, &mut iter);

        if held.is_null() {
            break;
        }

        // The wrapper's ints are all the dump of this struct would ever show us; the decode reads them
        // by name from `obscured_int_decode`.
        if unsafe { (*il2cpp_field_get_type(held)).type_() } != I4 {
            continue;
        }

        if let Some(name) = as_string(il2cpp_field_get_name(held)) {
            shape.push((name, il2cpp_field_get_offset(held)));
        }
    }

    let names: Vec<String> = shape.iter().map(|(name, _)| name.clone()).collect();
    *SP_SHAPE.lock().unwrap_or_else(|e| e.into_inner()) = shape;
    info!("Skill buy probe: SP field at offset {}, struct ints: {}", SP_FIELD_OFFSET.load(atomic::Ordering::Relaxed), names.join(", "));
}

pub fn init(umamusume: *const Il2CppImage) {
    if !Hachimi::instance().config.load().debug_mode {
        return;
    }

    let utils = class_for_label(umamusume, "Gallop.SingleModeUtils");
    let chara = class_for_label(umamusume, "Gallop.WorkSingleModeCharaData");

    let mut missing: Vec<&str> = Vec::new();
    let mut installed = 0usize;

    macro_rules! probe {
        ($class:expr, $hook:ident, $method:literal, $params:expr, $ret:expr, $label:literal) => {
            match $class {
                Some(class) => {
                    let addr = unsafe { AnimationSpeed::resolve_method(class, $method, $params, $ret) };

                    if addr != 0 {
                        new_hook!(addr, $hook);
                        installed += 1;
                    }
                    else {
                        missing.push($label);
                    }
                },
                None => missing.push($label),
            }
        };
    }

    macro_rules! static_probe {
        ($class:expr, $hook:ident, $method:literal, $params:expr, $ret:expr, $label:literal) => {
            match $class {
                Some(class) => {
                    let addr = unsafe { AnimationSpeed::resolve_static_method(class, $method, $params, $ret) };

                    if addr != 0 {
                        new_hook!(addr, $hook);
                        installed += 1;
                    }
                    else {
                        missing.push($label);
                    }
                },
                None => missing.push($label),
            }
        };
    }

    macro_rules! static_generic_probe {
        ($class:expr, $hook:ident, $method:literal, $params:expr, $ret:expr, $label:literal) => {
            match $class {
                Some(class) => {
                    let addr = unsafe { AnimationSpeed::resolve_static_generic_ref_method(class, $method, $params, $ret) };

                    if addr != 0 {
                        new_hook!(addr, $hook);
                        installed += 1;
                    }
                    else {
                        missing.push($label);
                    }
                },
                None => missing.push($label),
            }
        };
    }

    static_generic_probe!(utils, SingleModeUtils_CreateSkillInfoBySkillIdList, "CreateSkillInfoBySkillIdList", ONE_GENERIC, GENERIC, "SingleModeUtils::CreateSkillInfoBySkillIdList(ids)");
    static_generic_probe!(utils, SingleModeUtils_CalcNeedRareSkillPoint, "CalcNeedRareSkillPoint", ONE_GENERIC, VOID, "SingleModeUtils::CalcNeedRareSkillPoint(skills)");
    static_generic_probe!(utils, SingleModeUtils_GetTopIndexFromSkillLerningListItemInfoList, "GetTopIndexFromSkillLerningListItemInfoList", ONE_GENERIC, I4, "SingleModeUtils::GetTopIndexFromSkillLerningListItemInfoList(rows)");
    static_probe!(utils, SingleModeUtils_GetSkillFrameSprite, "GetSkillFrameSprite", ONE_MASTER_ROW, CLASS, "SingleModeUtils::GetSkillFrameSprite(masterRow)");
    probe!(chara, WorkSingleModeCharaData_GetTipsSkillPointDiscount, "GetTipsSkillPointDiscount", ONE_INT, I4, "WorkSingleModeCharaData::GetTipsSkillPointDiscount(skillId)");
    probe!(chara, WorkSingleModeCharaData_GetTipsSkillLevel, "GetTipsSkillLevel", ONE_INT, I4, "WorkSingleModeCharaData::GetTipsSkillLevel(skillId)");
    probe!(chara, WorkSingleModeCharaData_CalcNeedSkillPoint, "CalcNeedSkillPoint", TWO_INTS, I4, "WorkSingleModeCharaData::CalcNeedSkillPoint(a, b)");

    if let Some(class) = chara {
        read_sp_shape(class);
    }

    ARMED.store(true, atomic::Ordering::Release);
    info!("Skill buy probe: {installed} of {} doors installed, the screen's rows, prices, hint levels and SP are captured from the first visit", PROBES.len());

    if !missing.is_empty() {
        info!("Skill buy probe: not installed, no class or no matching overload: {}", missing.join(", "));
    }
}

// Called from the GameSystem update detour beside the other probe reports.
pub fn report_if_due() {
    if !ARMED.load(atomic::Ordering::Acquire) {
        return;
    }

    let totals: usize = PROBES.iter().map(|probe| probe.calls()).sum();
    let now_sec = ProbeSuite::now_us() / 1_000_000;
    let last_sec = LAST_REPORT_SEC.load(atomic::Ordering::Relaxed);
    let last_totals = LAST_TOTALS.load(atomic::Ordering::Relaxed);

    if !report_due(now_sec, last_sec, totals, last_totals, REPORT_INTERVAL_SECS) {
        return;
    }

    LAST_REPORT_SEC.store(now_sec, atomic::Ordering::Relaxed);
    LAST_TOTALS.store(totals, atomic::Ordering::Relaxed);

    let mut line = String::new();

    for probe in PROBES.iter() {
        let calls = probe.calls();

        if calls == 0 {
            continue;
        }

        let _ = write!(line, " {}={}", probe.label(), calls);
    }

    info!("Skill buy probe totals at {now_sec} s:{line}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unmasked_obscured_int_decodes_to_its_value_field() {
        assert_eq!(obscured_int_decode(450, 0x1234, None), 450);
        assert_eq!(obscured_int_decode(450, 0x1234, Some(0)), 450);
        assert_eq!(obscured_int_decode(450, 0x1234, Some(-1)), 450);
    }

    #[test]
    fn a_masked_obscured_int_decodes_to_the_xor_of_its_two_halves() {
        let value = 450 ^ 0x5A5A;
        assert_eq!(obscured_int_decode(value, 0x5A5A, Some(OBSCURED_MASKED)), 450);
        assert_eq!(obscured_int_decode(0, 0, Some(OBSCURED_MASKED)), 0);
    }

    #[test]
    fn every_buy_door_is_named_by_the_class_that_owns_it() {
        let labels: Vec<&str> = PROBES.iter().map(|probe| probe.label()).collect();

        assert_eq!(labels.len(), 7);
        assert!(labels.iter().all(|label| {
            label.starts_with("SingleModeUtils::") || label.starts_with("WorkSingleModeCharaData::")
        }));
    }

    #[test]
    fn the_caps_leave_room_for_one_full_screen_of_rows() {
        // A skill screen shows a screen's worth at a time; the row cap covers it, the field cap covers a
        // row wider than any list this fork has read, and a wider one says so instead of stopping.
        assert!(ROW_CAP >= 14);
        assert!(FIELD_CAP >= 24);
        assert!(STRING_CAP >= 32);
    }
}
