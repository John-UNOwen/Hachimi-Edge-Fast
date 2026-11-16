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
//! read by their runtime names, the raw ints are always logged, and the plain number is printed as
//! `SP <n>` only when the wrapper says it is holding the value in the clear.
//!
//! Nothing here changes an argument or a return value: every wrapper hands the call to the game
//! untouched. The doors are armed on every run, because the career dump button reads the owned
//! snapshots this module keeps: the screen's rows, the master names, and the per-skill tip
//! observations. `debug_mode` only decides whether the per-row log lines print, so a session with
//! it off stays quiet on the log and still fills the snapshots the dump writes to JSON.

use std::collections::{BTreeMap, BTreeSet};
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

// The doors are armed always; only the per-row lines follow `debug_mode`. Set once in `init`, read
// as a relaxed bool by every door: cheaper than the formatting the gate skips.
static VERBOSE: AtomicBool = AtomicBool::new(false);

fn verbose() -> bool {
    VERBOSE.load(atomic::Ordering::Relaxed)
}

// The SP read: offset of `field <SkillPoint>k__BackingField` on the character data, plus the int fields
// of the ObscuredInt struct it holds, by runtime name. Empty when the field name moved: the doors then
// report every other number and simply say the SP field is not there.
static SP_FIELD_OFFSET: AtomicUsize = AtomicUsize::new(0);
static SP_SHAPE: Mutex<Vec<(String, usize)>> = Mutex::new(Vec::new());

// ---- The career capture sink -------------------------------------------------------------------
//
// What the doors see, copied into owned Rust data the instant they see it. The dump button later
// serializes these snapshots and never touches a game pointer from them: the rows list, its Info
// objects and the master rows all belong to the skill screen and can be freed the moment that
// screen closes, so a snapshot taken while the door was firing is the only safe record of them.
// Bounded everywhere: rows by ROW_CAP, fields by FIELD_CAP, names by the caps below.

const MASTER_NAME_CAP: usize = 256;
const MASTER_STR_CAP: usize = 96;
const MASTER_STRINGS_PER_ROW: usize = 24;
const OBSERVED_SKILL_CAP: usize = 512;

#[derive(Clone, Default)]
pub(crate) struct ScreenSnap {
    pub(crate) rows: Vec<Vec<(String, i64)>>,
    pub(crate) top: i32,
    pub(crate) stamp_us: i64,
    // The character the rows were built for, read from the list's own object header when the door
    // fired. Kept as a marker only: the dump re-derives the live character from the state root and
    // never calls through this.
    pub(crate) chara: usize,
}

pub(crate) static SCREEN_SNAP: Mutex<ScreenSnap> = Mutex::new(ScreenSnap { rows: Vec::new(), top: 0, stamp_us: -1, chara: 0 });

// The character pointer, stamped by the doors whose `this` it is. Those doors are proven to fire
// on any skill screen visit, so on that screen the character itself is the freshest live career
// pointer there is; the career dump reads it before falling back to the state root.
pub(crate) static CHARA_AT: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CHARA_STAMP_US: AtomicI64 = AtomicI64::new(-1);

pub(crate) fn stamp_chara(chara: *mut Il2CppObject) {
    if chara.is_null() {
        return;
    }

    CHARA_STAMP_US.store(ProbeSuite::now_us(), atomic::Ordering::Relaxed);
    CHARA_AT.store(chara as usize, atomic::Ordering::Release);
}
pub(crate) static MASTER_NAMES: Mutex<BTreeMap<i32, Vec<(String, String)>>> = Mutex::new(BTreeMap::new());
pub(crate) static OBSERVED_SKILLS: Mutex<BTreeSet<i32>> = Mutex::new(BTreeSet::new());
// skill id -> (tip discount, hint level), each filled by whichever door reported it.
pub(crate) static TIPS_OBSERVED: Mutex<BTreeMap<i32, (Option<i32>, Option<i32>)>> = Mutex::new(BTreeMap::new());

/// The metadata name for a field, cut to what the game's own C# called it. Auto-properties store
/// their value in `<Name>k__BackingField`; private fields here carry no such prefix, so anything
/// else stays verbatim rather than guessing at a rename.
pub(crate) fn clean_field_name(name: &str) -> String {
    if let Some(inner) = name.strip_suffix(">k__BackingField") {
        if let Some(field) = inner.strip_prefix('<') {
            return field.to_owned();
        }
    }

    name.to_owned()
}

pub(crate) fn lock_or_recover<T>(cell: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    cell.lock().unwrap_or_else(|e| e.into_inner())
}

/// The integer an instance field holds, for the kinds a row can price or gate on. Anything else
/// answers None and is skipped: a captured row is numbers only, names and lists are walked by the
/// dump itself when it serializes the live object.
fn field_int(at: *const u8, kind: Il2CppTypeEnum) -> Option<i64> {
    Some(match kind {
        I4 => unsafe { *(at as *const i32) as i64 },
        Il2CppTypeEnum_IL2CPP_TYPE_U4 => unsafe { *(at as *const u32) as i64 },
        Il2CppTypeEnum_IL2CPP_TYPE_I2 => unsafe { *(at as *const i16) as i64 },
        Il2CppTypeEnum_IL2CPP_TYPE_U2 => unsafe { *(at as *const u16) as i64 },
        Il2CppTypeEnum_IL2CPP_TYPE_I1 => unsafe { *(at as *const i8) as i64 },
        Il2CppTypeEnum_IL2CPP_TYPE_U1 => unsafe { *(at as *const u8) as i64 },
        BOOL => { (unsafe { *(at as *const u8) } != 0) as i64 },
        Il2CppTypeEnum_IL2CPP_TYPE_I8 => unsafe { *(at as *const i64) },
        Il2CppTypeEnum_IL2CPP_TYPE_U8 => unsafe { *(at as *const u64) as i64 },
        _ => return None,
    })
}

/// Copy the screen's rows into owned data. Only while the door hands a list of references: an
/// unreadable or value-element list leaves the previous snapshot standing.
fn capture_screen_rows(rows_list: *mut Il2CppObject, top: i32) {
    if rows_list.is_null() {
        return;
    }

    let Some((items, count)) = list_backing(rows_list) else { return };
    let count = count.min(ROW_CAP);

    if count == 0 || !list_elements_are_refs(items) {
        return;
    }

    let capacity = il2cpp_array_length(items) as usize;
    let (data, stride) = array_data(items);
    let shown = count.min(capacity);
    let mut rows = Vec::new();

    for index in 0..shown {
        let element = unsafe { *((data).add(index * stride) as *mut *mut Il2CppObject) };

        if element.is_null() {
            continue;
        }

        let klass = il2cpp_object_get_class(element);
        let mut fields = Vec::new();
        let mut iter: *mut ::std::os::raw::c_void = std::ptr::null_mut();

        loop {
            let field = il2cpp_class_get_fields(klass, &mut iter);

            if field.is_null() {
                break;
            }

            if il2cpp_field_get_flags(field) & 0x10 != 0 || fields.len() >= FIELD_CAP {
                continue;
            }

            let kind = unsafe { (*il2cpp_field_get_type(field)).type_() };
            let at = unsafe { (element as *mut u8).add(il2cpp_field_get_offset(field)) };

            if let Some(value) = field_int(at, kind) {
                if let Some(name) = as_string(il2cpp_field_get_name(field)) {
                    fields.push((clean_field_name(&name), value));
                }
            }
        }

        rows.push(fields);
    }

    // The list object's own header names the class, and the class is what the SP shape was read
    // from; keep the character pointer only as a marker of whose screen this was.
    let chara = unsafe { *(rows_list as *mut usize) };
    let stamp = ProbeSuite::now_us();
    let mut snap = lock_or_recover(&SCREEN_SNAP);

    // The door is handed different windows as the screen scrolls, and a late call can carry a
    // 1-row remainder of a screen the player already left. Keep the fullest recent snapshot
    // (newer wins ties): the rows the player was looking at, not the last call.
    if rows.len() >= snap.rows.len() {
        snap.rows = rows;
        snap.top = top;
        snap.stamp_us = stamp;
        snap.chara = chara;
    }
}

/// The first sighting of a master row's Id: its string fields are the skill's display name. Rows
/// repeat on every cell bind, so the seen-Id guard keeps this to once per skill per run.
fn capture_master_name(row: *mut Il2CppObject) {
    if row.is_null() {
        return;
    }

    let klass = il2cpp_object_get_class(row);
    let id_field = il2cpp_class_get_field_from_name(klass, c"Id".as_ptr());
    let id_field = if id_field.is_null() { il2cpp_class_get_field_from_name(klass, c"<Id>k__BackingField".as_ptr()) } else { id_field };

    if id_field.is_null() || unsafe { (*il2cpp_field_get_type(id_field)).type_() } != I4 {
        return;
    }

    let id = unsafe { *( (row as *mut u8).add(il2cpp_field_get_offset(id_field)) as *const i32 ) };
    let mut names = lock_or_recover(&MASTER_NAMES);

    if names.contains_key(&id) || names.len() >= MASTER_NAME_CAP {
        return;
    }

    let mut strings = Vec::new();
    let mut iter: *mut ::std::os::raw::c_void = std::ptr::null_mut();
    let mut walked = 0usize;

    loop {
        let field = il2cpp_class_get_fields(klass, &mut iter);

        if field.is_null() || walked >= MASTER_CAP {
            break;
        }

        walked += 1;

        if il2cpp_field_get_flags(field) & 0x10 != 0 || strings.len() >= MASTER_STRINGS_PER_ROW {
            continue;
        }

        if unsafe { (*il2cpp_field_get_type(field)).type_() } != STRING {
            continue;
        }

        let held = unsafe { *( (row as *mut u8).add(il2cpp_field_get_offset(field)) as *mut *mut Il2CppString ) };

        if let Some(text) = unsafe { held.as_ref() } {
            let full = text.as_utf16str().to_string();

            // Empty strings are fields this row does not use; keeping them out is what leaves the
            // cap for the name the caller came for.
            if !full.is_empty() {
                if let Some(name) = as_string(il2cpp_field_get_name(field)) {
                    let short: String = full.chars().take(MASTER_STR_CAP).collect();
                    strings.push((clean_field_name(&name), short));
                }
            }
        }
    }

    names.insert(id, strings);
}

/// Record a skill id the screen offered. Bounded so a session that keeps opening screens cannot
/// grow the set without limit; the cap stops new ids, it never drops old ones.
fn capture_observed_ids(ids: *mut Il2CppObject) {
    if ids.is_null() {
        return;
    }

    let Some((items, count)) = list_backing(ids) else { return };
    let capacity = il2cpp_array_length(items) as usize;
    let (data, stride) = array_data(items);
    let shown = count.min(capacity).min(OBSERVED_SKILL_CAP);

    // An id list is ints in the array data, never element pointers.
    if list_elements_are_refs(items) {
        return;
    }

    let mut observed = lock_or_recover(&OBSERVED_SKILLS);

    for index in 0..shown {
        let held = unsafe { *(((data).add(index * stride)) as *const i32) };

        if observed.len() < OBSERVED_SKILL_CAP {
            observed.insert(held);
        }
    }
}

/// Fold one tip observation into the map. Pure over owned data so the merge rule is testable
/// without the game: discount and level fill apart, and a later fill never erases the earlier one.
pub(crate) fn observe_tips(observed: &mut BTreeMap<i32, (Option<i32>, Option<i32>)>, skill_id: i32, value: i32, is_level: bool) {
    if observed.len() >= OBSERVED_SKILL_CAP && !observed.contains_key(&skill_id) {
        return;
    }

    let entry = observed.entry(skill_id).or_insert((None, None));

    if is_level { entry.1 = Some(value); } else { entry.0 = Some(value); }
}

// How many rows the element dumps print in full, and how wide each object level goes. A row that
// outgrows these is counted and named, never silently half-printed. The master-data row is the one
// object dumped wider than a screen row: its name and rate fields sit past a screen row's width,
// and it prints once per skill rather than once per field.
const FIELD_CAP: usize = 24;
const MASTER_CAP: usize = 80;
const ROW_CAP: usize = 14;
const STRING_CAP: usize = 64;
// A nested object (a row's MasterData, a list element) is walked only below its owner and only a
// level or two down: the fields that matter sit on the first level, and an unbounded walk could
// follow any object graph off across the log line.
const NEST_CAP: usize = 16;
const NEST_DEPTH: usize = 2;
const NEST_LIST_MAX: usize = 4;

/// The plaintext rule for the anti-cheat int this client uses. The live dump of the SP field named
/// its ints `cryptoKey, currentCryptoKey, hiddenValue, fakeValue`: while `currentCryptoKey` is
/// zero the number is held in the clear and `fakeValue` is it. Once a key is current, the real
/// number sits obfuscated inside `hiddenValue` behind the key, and this answers `None`: the line
/// says "held obscured" rather than guessing, and the raw fields are logged either way.
pub(crate) fn obscured_int_plain(current_crypto_key: i32, fake_value: i32) -> Option<i32> {
    (current_crypto_key == 0).then_some(fake_value)
}

fn sp_text(chara: *mut Il2CppObject) -> String {
    let offset = SP_FIELD_OFFSET.load(atomic::Ordering::Relaxed);

    if offset == 0 || chara.is_null() {
        return String::from(" SP field not read");
    }

    let base = unsafe { (chara as *mut u8).add(offset) };
    let shape = SP_SHAPE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let mut out = String::from(" SP raw[");
    let mut crypto_current: Option<i32> = None;
    let mut fake: Option<i32> = None;

    for (name, field_offset) in &shape {
        let held = unsafe { *(base.add(*field_offset) as *const i32) };
        let _ = write!(out, " {name}={held}");

        if name.eq_ignore_ascii_case("currentCryptoKey") { crypto_current = Some(held); }
        if name.eq_ignore_ascii_case("fakeValue") { fake = Some(held); }
    }

    out.push(']');

    if let (Some(key), Some(value)) = (crypto_current, fake) {
        match obscured_int_plain(key, value) {
            Some(plain) => { let _ = write!(out, " SP {plain}"); },
            None => out.push_str(" SP held obscured"),
        }
    }

    out
}

/// `name=value` for one instance field of a live object, at one level of the walk. Every primitive
/// the metadata can name is read; strings are printed truncated; an object field is walked one
/// level down while the depth allows; a generic `List<T>` is expanded only while it is short, the
/// depth allows, and its element class is proven a reference by the runtime itself. A null object
/// or field prints its own marker instead of reading through it.
fn field_value_text(obj: *mut Il2CppObject, field: *mut FieldInfo, depth: usize) -> String {
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

            if !type_name.contains("System.Collections.Generic.List") {
                return format!("<{type_name}>");
            }

            list_value_text(held, depth)
        },
        VALUETYPE | ENUM => {
            let class = il2cpp_type_get_class_or_element_class(il2cpp_field_get_type(field));
            let name = if class.is_null() { "?".to_owned() } else { as_string(il2cpp_class_get_name(class)).unwrap_or_else(|| "?".to_owned()) };

            format!("<struct {name}>")
        },
        CLASS | Il2CppTypeEnum_IL2CPP_TYPE_OBJECT | Il2CppTypeEnum_IL2CPP_TYPE_SZARRAY | Il2CppTypeEnum_IL2CPP_TYPE_ARRAY => {
            let held = unsafe { *(at as *mut *mut c_void) };

            if held.is_null() {
                String::from("<null>")
            }
            else if depth < NEST_DEPTH {
                let mut walked = String::new();
                dump_fields_at(&mut walked, held as *mut Il2CppObject, NEST_CAP, depth + 1);
                walked
            }
            else {
                String::from("<obj>")
            }
        },
        _ => String::from("<skipped>"),
    }
}

pub(crate) fn as_string(ptr: *const ::std::os::raw::c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }

    unsafe { std::ffi::CStr::from_ptr(ptr) }.to_str().ok().map(str::to_owned)
}

/// The live size of a `List<T>`, read through its own `_size` field. A list whose shape this client
/// spells differently reports as unreadable rather than as empty.
pub(crate) fn list_size(list: *mut Il2CppObject) -> Option<usize> {
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

/// The element array of a live list and the element count the list itself reports. Null slots are
/// the caller's to notice; an unreadable shape answers `None` rather than an empty list.
pub(crate) fn list_backing(list: *mut Il2CppObject) -> Option<(*mut Il2CppArray, usize)> {
    let count = list_size(list)?;
    let items_field = il2cpp_class_get_field_from_name(il2cpp_object_get_class(list), c"_items".as_ptr());

    if items_field.is_null() {
        return None;
    }

    let items = unsafe { *( (list as *mut u8).add(il2cpp_field_get_offset(items_field)) as *mut *mut Il2CppArray ) };

    if items.is_null() {
        return None;
    }

    Some((items, count))
}

/// The data area of a reference array and the element stride, both as the runtime itself reports
/// them. The stride is read, never assumed: one build's pointer is not another's slot size.
pub(crate) fn array_data(items: *mut Il2CppArray) -> (*mut u8, usize) {
    let stride = il2cpp_array_element_size(il2cpp_object_get_class(items as *mut Il2CppObject)) as usize;

    (unsafe { (items as *mut u8).add(il2cpp_array_object_header_size() as usize) }, stride)
}

/// Whether a list's elements are references the dump may walk as objects. The array's own element
/// class is asked of the runtime: value elements stay counts, because reading one as a pointer
/// would dereference a number as an address.
pub(crate) fn list_elements_are_refs(items: *mut Il2CppArray) -> bool {
    let element = il2cpp_class_get_element_class(il2cpp_object_get_class(items as *mut Il2CppObject));

    !element.is_null() && !il2cpp_class_is_valuetype(element as *const Il2CppClass)
}

/// `List[n items]`, or the elements themselves while the list is short enough, shallow enough,
/// and provably of references to expand without burying the line or reading anything wrong.
fn list_value_text(list: *mut Il2CppObject, depth: usize) -> String {
    let Some(count) = list_size(list) else { return String::from("[unreadable list]") };

    if count == 0 || count > NEST_LIST_MAX || depth >= NEST_DEPTH {
        return format!("List[{count} items]");
    }

    let Some((items, _)) = list_backing(list) else {
        return format!("List[{count} items]");
    };

    if !list_elements_are_refs(items) {
        return format!("List[{count} items]");
    }

    let mut out = format!("List[{count}]");
    let capacity = il2cpp_array_length(items) as usize;
    let (data, stride) = array_data(items);
    let shown = count.min(capacity);

    for index in 0..shown {
        let element = unsafe { *((data).add(index * stride) as *mut *mut Il2CppObject) };
        dump_fields_at(&mut out, element, NEST_CAP, depth + 1);
    }

    out
}

/// Walk a live `List<int>`. The ints live in the array data itself, not behind element pointers, so
/// reading them as pointers would dereference a small number as an address.
fn dump_int_list(out: &mut String, list: *mut Il2CppObject) {
    if list.is_null() {
        out.push_str("<null list>");
        return;
    }

    let Some((items, count)) = list_backing(list) else {
        out.push_str("<unreadable list>");
        return;
    };

    let capacity = il2cpp_array_length(items) as usize;
    let (data, stride) = array_data(items);
    let shown = count.min(capacity).min(ROW_CAP);

    let _ = write!(out, "[{count} ids");

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

    let Some((items, count)) = list_backing(list) else {
        out.push_str("<unreadable list>");
        return;
    };

    let _ = write!(out, "[{count} rows]");

    let capacity = il2cpp_array_length(items) as usize;
    let (data, stride) = array_data(items);
    let shown = count.min(capacity).min(ROW_CAP);

    for index in 0..shown {
        let element = unsafe { *((data).add(index * stride) as *mut *mut Il2CppObject) };

        let _ = write!(out, " row{index}:");
        dump_fields_at(out, element, FIELD_CAP, 0);
    }

    if count > shown {
        let _ = write!(out, " +{} rows over the cap", count - shown);
    }
}

/// The walk: a named field cap and the depth this object sits at, so a nested object
/// prints narrower than its owner and the object graph cannot outrun the line.
fn dump_fields_at(out: &mut String, obj: *mut Il2CppObject, cap: usize, depth: usize) {
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

        if printed >= cap {
            continue;
        }

        printed += 1;

        let name = as_string(il2cpp_field_get_name(field)).unwrap_or_else(|| "?".to_owned());
        let _ = write!(out, " {name}={}", field_value_text(obj, field, depth));
    }

    out.push('}');

    if total > printed {
        let _ = write!(out, " +{} fields over the cap", total - printed);
    }
}

/// A counted door whose lines stop at a caller-named limit. Formatting runs only while lines are
/// still owed and `debug_mode` is on; the count itself always runs, because the totals line is how
/// a normal run answers whether a door was called at all.
fn detail_line(door: &CutProbe, limit: usize, text: impl FnOnce() -> String) {
    door.count();

    if verbose() && door.calls() <= limit {
        info!("Skill buy probe: {}", text());
    }
}

type CreateSkillInfoFn = extern "C" fn(ids: *mut Il2CppObject) -> *mut Il2CppObject;
def_detour! {
    SingleModeUtils_CreateSkillInfoBySkillIdList(ids: *mut Il2CppObject) -> *mut Il2CppObject {
            CREATE_SKILL_INFO.count();

        let result = get_orig_fn!(SingleModeUtils_CreateSkillInfoBySkillIdList, CreateSkillInfoFn)(ids);

        capture_observed_ids(ids);

        if verbose() && CREATE_SKILL_INFO.calls() <= PROBE_DETAIL_LIMIT {
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
        if verbose() && CALC_RARE.calls() <= PROBE_DETAIL_LIMIT {
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

        capture_screen_rows(rows, top);

        if verbose() && TOP_INDEX.calls() <= PROBE_DETAIL_LIMIT {
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

        // The capture reads this row's own fields flat: Id plus its non-empty strings, which is the
        // skill's name and tags. The deeper walk this door used to log was dropped on purpose: a
        // live run showed a field behind a pointer the barrier only survives by answering null
        // (`Hook barrier had no answer for SingleModeUtils_GetSkillFrameSprite`), and everything the
        // walk printed, the capture and the dump now keep in owned data.
        capture_master_name(row);

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

        stamp_chara(this);
        observe_tips(&mut lock_or_recover(&TIPS_OBSERVED), skill_id, result, false);

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

        stamp_chara(this);
        observe_tips(&mut lock_or_recover(&TIPS_OBSERVED), skill_id, result, true);

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

        // The wrapper's instance ints are all the dump of this struct would ever show us; the
        // decode reads them by name from `obscured_int_plain`. Statics are skipped outright: their
        // offsets point into the class's static block, and reading one at `this + offset` would
        // read a neighbour's bytes and call them the key.
        if il2cpp_field_get_flags(held) & 0x10 != 0 {
            continue;
        }

        if unsafe { (*il2cpp_field_get_type(held)).type_() } != I4 {
            continue;
        }

        if let Some(name) = as_string(il2cpp_field_get_name(held)) {
            shape.push((name, il2cpp_field_get_offset(held)));
        }
    }

    let names: Vec<String> = shape.iter().map(|(name, _)| name.clone()).collect();
    *SP_SHAPE.lock().unwrap_or_else(|e| e.into_inner()) = shape;

    if verbose() {
        info!("Skill buy probe: SP field at offset {}, struct ints: {}", SP_FIELD_OFFSET.load(atomic::Ordering::Relaxed), names.join(", "));
    }
}

pub fn init(umamusume: *const Il2CppImage) {
    // Armed always: the career dump button reads the snapshots these doors keep. Only the per-row
    // lines follow `debug_mode`, so a quiet run pays a relaxed load per door and nothing else.
    VERBOSE.store(Hachimi::instance().config.load().debug_mode, atomic::Ordering::Relaxed);

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
    info!("Skill buy probe: {installed} of {} doors armed; the career dump reads their snapshots, verbose lines follow debug_mode", PROBES.len());

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
    fn a_plain_obscured_int_reads_through_its_fake_value_field() {
        // The live line this rule came from: currentCryptoKey=0 hiddenValue=0 fakeValue=500.
        assert_eq!(obscured_int_plain(0, 500), Some(500));
    }

    #[test]
    fn an_obscured_int_decodes_to_nothing_rather_than_a_guess() {
        assert_eq!(obscured_int_plain(165158, 0), None);
        assert_eq!(obscured_int_plain(-1, 450), None);
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

    #[test]
    fn a_backing_field_is_named_as_the_property_the_dump_asks_for() {
        assert_eq!(clean_field_name("<SkillId>k__BackingField"), "SkillId");
        assert_eq!(clean_field_name("DiscountRateForDisplay"), "DiscountRateForDisplay");
        // A dangling suffix with no opening angle is not a backing field and keeps its name.
        assert_eq!(clean_field_name("xk__BackingField"), "xk__BackingField");
    }

    #[test]
    fn tip_discount_and_hint_level_fill_the_same_entry_apart() {
        let mut observed = BTreeMap::new();

        observe_tips(&mut observed, 201902, 40, false);
        observe_tips(&mut observed, 201902, 3, true);

        assert_eq!(observed.get(&201902), Some(&(Some(40), Some(3))));

        // A door that reports again replaces its own half, never the other one.
        observe_tips(&mut observed, 201902, 20, false);
        assert_eq!(observed.get(&201902), Some(&(Some(20), Some(3))));
    }

    #[test]
    fn the_observed_set_stops_growing_at_its_cap_rather_than_dropping_what_it_holds() {
        let mut observed: BTreeMap<i32, (Option<i32>, Option<i32>)> = BTreeMap::new();

        for id in 0..(OBSERVED_SKILL_CAP as i32 + 10) {
            observe_tips(&mut observed, id, 1, false);
        }

        assert_eq!(observed.len(), OBSERVED_SKILL_CAP);
        assert!(observed.contains_key(&0));
        assert!(!observed.contains_key(&(OBSERVED_SKILL_CAP as i32)));
    }
}
