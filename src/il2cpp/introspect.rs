//! Debug-only metadata dump used to locate hook points on a build whose API shape is
//! not known yet.
//!
//! Hachimi binds game functions by (class, name, parameter count) through
//! `get_method_addr`, so all that is needed to target an unknown build is the real set
//! of names it exposes. With `debug_mode` on, this walks every class in the game's own
//! assemblies once and writes the matches to `<data dir>/introspect.log`, which is read
//! offline to pick hooks instead of guessing names (a wrong name never fails loudly, it
//! just resolves to nothing and logs "Failed to resolve").
//!
//! The pass is metadata only: it installs no hooks and never touches live object
//! instances, and it runs inside an SEH guard so a bad pointer cannot take the game down.
//!
//! C44: the dump is a snapshot of the *installed client's* metadata, so the same client
//! produces byte for byte the same 1.9 MB file launch after launch, written from inside the
//! loader lock window (C25). It is therefore written once and reused. The first launch that
//! finds no current file stamps it with the metadata and the dump rules it came from; every
//! later launch folds the image headers (names and class counts, no class walk), compares the
//! stamp line at the head of the file and the completion line at its tail, and writes nothing
//! at all. A client update that moves an image name, a class count or the client executable
//! re-dumps, and so does any change to the filters, the caps or the allowlist below, because
//! those are part of the stamp too. Deleting `introspect.log` forces a fresh dump. A dump
//! whose walk faulted never gets its completion line, so a partial log is never reused.
//!
//! The reused file describes the launch that wrote it: the static field values it prints are
//! the values that launch saw.

use std::ffi::{CStr, c_char};
use std::fs::{File, create_dir_all};
use std::io::{self, BufWriter, Read, Seek, Write};
use std::os::raw::c_void;
use std::path::Path;
use std::ptr;

use crate::core::Hachimi;
use crate::il2cpp::api::*;
use crate::il2cpp::types::*;

/// Only these assemblies are walked: the game's own code plus its animation middleware.
/// Unity and .NET assemblies are skipped because their API is already known here.
const IMAGE_FILTERS: &[&str] = &[
    "umamusume", "animatetounity", "cute_", "dotween", "assembly-csharp", "cyspring"
];

/// A class whose name matches one of these is dumped in full: every method with its
/// parameter count, return type and parameter types, and every field with its type,
/// static/const flags and current static value. Kept narrow on purpose; breadth comes
/// from the method and field scans below, which label every hit with its declaring class.
const CLASS_FILTERS: &[&str] = &[
    "uimanager", "scenemanager", "viewmanager", "viewcontroller", "nowloading", "connecting",
    "transition", "fade", "storyview", "storytext", "storytimeline", "trainingparam",
    "trainingresult", "tapeffect", "anroot", "anmotion", "animotion", "director",
    "result", "orientation", "reward", "countup"
];

/// Every other class is scanned for method names that look like a duration knob, since
/// those are the ones worth multiplying.
const METHOD_FILTERS: &[&str] = &[
    "fade", "transition", "duration", "timescale", "time_scale", "speed", "skip", "fast",
    "delay", "wait", "plate", "changescene", "changeview", "nextscene", "playrate",
    "elapsedtime", "currenttime", "settime", "curspeed", "interval", "cutt", "cutin"
];

/// Same for field names: a public backing field is often the only way to reach a duration.
const FIELD_FILTERS: &[&str] = &[
    "duration", "timescale", "time_scale", "speed", "fade", "transition", "delay",
    "elapsedtime", "curtime", "curspeed", "wait", "interval"
];

/// Enough to cover a transition API without the log becoming unreadable. Truncation is
/// reported instead of silently dropping the rest.
const MAX_HITS: usize = 30_000;

/// Cap on full class dumps, so a broad filter cannot turn the log into the whole assembly.
/// The Global client spends it: the captured dump in `run log\introspect.log` holds 534 full
/// blocks, 500 general plus 34 from the allowlist budget, and 1,253 distinct classes appear
/// there as filter hits with no block of their own, while the walk went on past the cap. That
/// is A8 and A26. The cap stays where it is; what changed is that the log now says it was
/// reached (see `full_class_cap_note`), so a name missing from a dump can be told apart from
/// a name the cap passed over.
const MAX_FULL_CLASSES: usize = 500;

/// Classes dumped in full by exact name instead of by substring. The training cut-in work needs the
/// whole surface of these, and the general cap is spent long before the walk reaches them: in a
/// career run's log the 500th full dump landed at line 23504 of a walk that ran to 27524 (A26).
const FULL_DUMP_NAMES: &[&str] = &[
    "SingleModeMainTrainingCuttController", "TimelineTrainingCuttController",
    "SingleModeTrainingCutInHelper", "SingleModeTrainingCutHelperExtension",
    "TagTrainingCutInPlayer", "SingleModeMainViewTagTrainingCutInPlayer",
    "SingleModeMainViewTrainingCutStatus", "SingleModeMainViewTrainingCutStatusFrame",
    "SingleModeMainViewTrainingFooter", "SingleModeMainViewHpGauge", "SingleModeUtils",
    "SingleModeDefine", "SingleModeMainDefine", "SingleModeMainViewController",
    "SingleModeMainHeaderAndFooterController", "TrainingParamChangeUI", "CutInTimelineController",
    "CutInHelper", "CutInBgModel", "SingleModeLogItem", "SingleModeLogGroupBase",
    // The story side. These are the classes a story event option has to be written against, and no dump
    // has ever printed their signatures, which is why the story event probe measures the cut-in doors the
    // dump does name and not these (C47: a probe on a guessed signature measures nothing).
    "StoryViewController", "StorySceneController", "StoryEventMissionViewController",
    "StoryCharacterFade", "StoryTimelineController",
    // The coroutines the training screen waits on. A compiler generated state machine appears in a log only
    // as the owner of whichever of its captured locals matched a field filter, so its methods, its branch
    // marker and the locals no filter names have never been printed. These are the names the career run of
    // 2026-10-09 printed on their field reference lines, spelled exactly as that client spells them, and the
    // first half of C58 has to be read out of them.
    "<PlayTrainingCut>d__70", "<PlayCutt>d__52", "<PlayFlashAndTypewriter>d__13",
    "<InitializeFlash>d__109", "<PlayParameterChangeAsync>d__350", "<InitializeEachPlayIn>d__13",
    "<InitializeEachPlayIn>d__14", "<PlayTrainingTipsEventWipe>d__52", "<PlayGaugeUpAnimation>d__33",
    "<PlayGaugeUpAnimation>d__38", "<CoroutineGaugeUpAnimation>d__21", "<CoroutineGaugeUpAnimation>d__43",
    "<CoroutineAppendParamUpResultSequence>d__8", "<PlayResultCutinCoroutine>d__46",
];

/// Allowlisted classes get their own budget so a spent general cap cannot hide them. The list above
/// is bounded, so the log grows by these classes and not by whatever else matches a filter. It is now the
/// whole list, so the budget carries a little headroom: a client that renames a state machine takes the
/// slot rather than crowding a name this fork is measured against out.
const MAX_ALLOWLIST_CLASSES: usize = 44;

/// Shape of the dump itself, stamped into the file. Bump it when the log gains or loses a
/// kind of line so an older file fails the comparison and is rewritten rather than reused as
/// if it were current.
const DUMP_FORMAT: u32 = 1;

/// Last line of a dump that finished. A walk that faulted, or a process killed before the
/// writer flushed, leaves the file without it, and a file without it is never reused.
const FOOTER_MARKER: &str = "# introspect complete";

/// How much of an existing log a launch reads to decide whether it is still current: the
/// first line and the last few bytes. The 1.9 MB between them is never read (C44).
const HEAD_BYTES: u64 = 512;
const TAIL_BYTES: u64 = 256;

/// One assembly the dump walks, held with what the cheap stamp pass needs from it: the name
/// the scope rule matched and the class count. The scope rule is read once here so the stamp
/// pass and the dump pass cannot drift apart.
struct ScopeImage {
    image: *const Il2CppImage,
    name: String,
    class_count: usize,
}

/// What the metadata walk is scoped to: the in-scope images in domain order, plus the reason
/// there are none when the domain is not there yet.
struct Scope {
    images: Vec<ScopeImage>,
    missing: Option<&'static str>,
}

/// The scope, read from the image headers only: no class, method or field walk. On a Global
/// client run that is six images, so a launch that goes on to reuse its dump pays this pass
/// and nothing else.
fn collect_scope() -> Scope {
    let mut scope = Scope { images: Vec::new(), missing: None };

    let domain = il2cpp_domain_get();
    if domain.is_null() {
        scope.missing = Some("no IL2CPP domain");
        return scope;
    }

    let mut size: usize = 0;
    let assemblies = il2cpp_domain_get_assemblies(domain as *const Il2CppDomain, &mut size);
    if assemblies.is_null() {
        scope.missing = Some("no assemblies");
        return scope;
    }

    for i in 0..size {
        let assembly = unsafe { *assemblies.add(i) };
        if assembly.is_null() {
            continue;
        }

        let image = il2cpp_assembly_get_image(assembly);
        if image.is_null() {
            continue;
        }

        let name = as_string(il2cpp_image_get_name(image)).unwrap_or_default();
        if !image_is_in_scope(&name) {
            continue;
        }

        scope.images.push(ScopeImage { image, name, class_count: il2cpp_image_get_class_count(image) });
    }

    scope
}

/// FNV-1a 64 bit fold, spelled out rather than a std hasher: the value has to mean the same
/// thing to the build that wrote the file and to the build that reads it next week.
fn fold(hash: &mut u64, text: &str) {
    for byte in text.bytes() {
        *hash ^= byte as u64;
        *hash = hash.wrapping_mul(0x100_0000_01b3);
    }
}

/// Fold of everything the dump code itself decides - the filters, the caps, the allowlist -
/// so that changing a filter or adding an allowlist name re-dumps instead of leaving a launch
/// to reuse a log that never contained the class the change was written for.
fn fold_dump_rules(hash: &mut u64) {
    fold(hash, &DUMP_FORMAT.to_string());

    for filter in IMAGE_FILTERS { fold(hash, filter); }
    fold(hash, "|");
    for filter in CLASS_FILTERS { fold(hash, filter); }
    fold(hash, "|");
    for filter in METHOD_FILTERS { fold(hash, filter); }
    fold(hash, "|");
    for filter in FIELD_FILTERS { fold(hash, filter); }
    fold(hash, "|");
    for name in FULL_DUMP_NAMES { fold(hash, name); }

    fold(hash, &MAX_HITS.to_string());
    fold(hash, &MAX_FULL_CLASSES.to_string());
    fold(hash, &MAX_ALLOWLIST_CLASSES.to_string());
}

/// The installed client this metadata belongs to: the game executable's file name and size.
/// Size and not mtime, because a launcher or a repair pass may touch a timestamp without
/// changing the client. On Android `current_exe` is the runtime binary, so the image headers
/// carry the identity there.
fn client_identity() -> String {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(_) => return "unknown".to_owned()
    };

    let name = match exe.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => "unknown".to_owned()
    };

    match exe.metadata() {
        Ok(meta) => format!("{name}:{}", meta.len()),
        Err(_) => name
    }
}

/// Identity of the metadata a dump is a snapshot of: the in-scope images with their class
/// counts, the rules that produced the dump, and the client executable. A client update that
/// adds, removes or renames a class moves a class count or an image name. One that changes
/// nothing the dump can see does not, and deleting `introspect.log` is then the documented
/// way to force a fresh dump.
struct DumpStamp {
    images: usize,
    classes: usize,
    client: String,
    hash: u64,
}

impl DumpStamp {
    /// Reads `images` for its name and class count only; the image pointers are not touched,
    /// which is what lets a test build a scope out of null pointers.
    fn of(images: &[ScopeImage], client: &str) -> Option<DumpStamp> {
        if images.is_empty() {
            // No metadata to identify: there is nothing safe to reuse, and a dump written
            // without a stamp line is never treated as current.
            return None;
        }

        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut classes = 0usize;

        fold_dump_rules(&mut hash);
        fold(&mut hash, client);
        for image in images {
            fold(&mut hash, &image.name);
            fold(&mut hash, &image.class_count.to_string());
            classes += image.class_count;
        }

        Some(DumpStamp { images: images.len(), classes, client: client.to_owned(), hash })
    }

    /// The first line of the log. A file counts as current only when its first line is this
    /// line, character for character, so the comparison needs no parsing.
    fn header_line(&self) -> String {
        format!(
            "# introspect dump v{} stamp {:016x} images {} classes {} full_class_cap {} hit_cap {} client {}",
            DUMP_FORMAT,
            self.hash,
            self.images,
            self.classes,
            MAX_FULL_CLASSES,
            MAX_HITS,
            self.client
        )
    }
}

/// The two ends of an existing log: its size, its first `HEAD_BYTES` and its last
/// `TAIL_BYTES`. Bounded on purpose - the point of the check is that a launch does not read
/// the body it is about to reuse.
fn head_and_tail<R: Read + Seek>(reader: &mut R) -> io::Result<(u64, String, String)> {
    let mut head = vec![0u8; HEAD_BYTES as usize];
    let read = reader.read(&mut head)?;
    head.truncate(read);

    let size = reader.seek(io::SeekFrom::End(0))?;
    let tail_len = size.min(TAIL_BYTES);
    reader.seek(io::SeekFrom::End(-(tail_len as i64)))?;

    let mut tail = vec![0u8; tail_len as usize];
    let read = reader.read(&mut tail)?;
    tail.truncate(read);

    Ok((size, String::from_utf8_lossy(&head).into_owned(), String::from_utf8_lossy(&tail).into_owned()))
}

/// True when the ends of an existing log say it is a complete dump of exactly this metadata
/// under exactly these rules.
fn dump_is_current(head: &str, tail: &str, stamp: &DumpStamp) -> bool {
    head.lines().next() == Some(stamp.header_line().as_str()) && tail.contains(FOOTER_MARKER)
}

/// Size in bytes of an existing log that is still current for this stamp, or None when there
/// is no such file. Opens, reads the two ends, closes.
fn existing_dump_size_if_current(path: &Path, stamp: &DumpStamp) -> Option<u64> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return None
    };

    let (size, head, tail) = match head_and_tail(&mut file) {
        Ok(found) => found,
        Err(_) => return None
    };

    if dump_is_current(&head, &tail, stamp) { Some(size) } else { None }
}

fn as_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }

    unsafe { CStr::from_ptr(ptr) }.to_str().ok().map(str::to_owned)
}

fn matches_any(name: &str, filters: &[&str]) -> bool {
    let lower = name.to_ascii_lowercase();
    filters.iter().any(|f| lower.contains(*f))
}

fn image_is_in_scope(name: &str) -> bool {
    matches_any(name, IMAGE_FILTERS)
}

const FIELD_ATTRIBUTE_STATIC: ::std::os::raw::c_int = 0x10;
const FIELD_ATTRIBUTE_PUBLIC: ::std::os::raw::c_int = 0x06;
const FIELD_ATTRIBUTE_INIT_ONLY: ::std::os::raw::c_int = 0x20;
const FIELD_ATTRIBUTE_LITERAL: ::std::os::raw::c_int = 0x40;
const METHOD_ATTRIBUTE_STATIC: u16 = 0x0010;

/// Readable name for a metadata type. The shape matters as much as the name: a wrapper
/// that assumes `float` for a method which actually returns `int` hands garbage to every
/// caller, so signatures are dumped rather than inferred from a method name.
fn type_label(t: *const Il2CppType) -> String {
    if t.is_null() {
        return "?".to_owned();
    }

    let kind = unsafe { (*t).type_() };
    let byref = unsafe { (*t).byref() } != 0;

    let name = match kind {
        Il2CppTypeEnum_IL2CPP_TYPE_VOID => "void",
        Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN => "bool",
        Il2CppTypeEnum_IL2CPP_TYPE_CHAR => "char",
        Il2CppTypeEnum_IL2CPP_TYPE_I1 => "sbyte",
        Il2CppTypeEnum_IL2CPP_TYPE_U1 => "byte",
        Il2CppTypeEnum_IL2CPP_TYPE_I2 => "short",
        Il2CppTypeEnum_IL2CPP_TYPE_U2 => "ushort",
        Il2CppTypeEnum_IL2CPP_TYPE_I4 => "int",
        Il2CppTypeEnum_IL2CPP_TYPE_U4 => "uint",
        Il2CppTypeEnum_IL2CPP_TYPE_I8 => "long",
        Il2CppTypeEnum_IL2CPP_TYPE_U8 => "ulong",
        Il2CppTypeEnum_IL2CPP_TYPE_R4 => "float",
        Il2CppTypeEnum_IL2CPP_TYPE_R8 => "double",
        Il2CppTypeEnum_IL2CPP_TYPE_STRING => "string",
        Il2CppTypeEnum_IL2CPP_TYPE_PTR => "ptr",
        Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE => "struct",
        Il2CppTypeEnum_IL2CPP_TYPE_CLASS => "class",
        Il2CppTypeEnum_IL2CPP_TYPE_ARRAY => "array",
        Il2CppTypeEnum_IL2CPP_TYPE_SZARRAY => "[]",
        Il2CppTypeEnum_IL2CPP_TYPE_GENERICINST => "generic",
        Il2CppTypeEnum_IL2CPP_TYPE_ENUM => "enum",
        Il2CppTypeEnum_IL2CPP_TYPE_OBJECT => "object",
        _ => "other",
    };

    let detail = match kind {
        // A `struct` argument is only safe to wrap as an integer when it is small enough
        // to travel in a general-purpose register, so the concrete type and its size are
        // dumped next to the keyword.
        Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE | Il2CppTypeEnum_IL2CPP_TYPE_ENUM | Il2CppTypeEnum_IL2CPP_TYPE_GENERICINST => {
            let class = il2cpp_type_get_class_or_element_class(t);

            if class.is_null() {
                String::new()
            }
            else {
                let name = as_string(il2cpp_type_get_name(t)).unwrap_or_else(|| "?".to_owned());
                // instance_size counts the object header; the payload is what the ABI moves.
                let size = il2cpp_class_instance_size(class) - 16;

                format!("<{name}:{size}B>")
            }
        },
        Il2CppTypeEnum_IL2CPP_TYPE_CLASS | Il2CppTypeEnum_IL2CPP_TYPE_OBJECT | Il2CppTypeEnum_IL2CPP_TYPE_STRING => {
            let name = as_string(il2cpp_type_get_name(t)).unwrap_or_else(|| "?".to_owned());
            format!("<{name}>")
        },
        _ => String::new(),
    };

    if byref { format!("{name}{detail}&") }
    else { format!("{name}{detail}") }
}

fn method_signature(method: *const MethodInfo) -> String {
    let return_type = type_label(il2cpp_method_get_return_type(method));
    let count = unsafe { (*method).parameters_count } as u32;

    let mut params = String::new();
    for i in 0..count {
        if i > 0 {
            params.push_str(", ");
        }
        params.push_str(&type_label(il2cpp_method_get_param(method, i)));
    }

    // Whether the method carries a hidden `this`. A wrapper that reserves one for a
    // static method reads every argument from the wrong register.
    let qualifier = if unsafe { (*method).flags } & METHOD_ATTRIBUTE_STATIC != 0 { "static " } else { "" };

    format!("{qualifier}{return_type}({params})")
}

fn field_signature(field: *mut FieldInfo) -> String {
    let flags = il2cpp_field_get_flags(field);
    let mut out = String::new();

    if flags & FIELD_ATTRIBUTE_PUBLIC != 0 {
        out.push_str("public ");
    }
    if flags & FIELD_ATTRIBUTE_STATIC != 0 {
        out.push_str("static ");
    }
    // `static readonly` is what a shipped duration constant normally is; a plain
    // `static` is a variable the game itself assigns.
    if flags & FIELD_ATTRIBUTE_INIT_ONLY != 0 {
        out.push_str("readonly ");
    }
    if flags & FIELD_ATTRIBUTE_LITERAL != 0 || il2cpp_field_is_literal(field) {
        out.push_str("const ");
    }

    out.push_str(&type_label(il2cpp_field_get_type(field)));
    out
}

/// Current value of a static primitive field. A class whose static constructor has not
/// run yet reads back as zero, which is itself the answer to "can I patch this now?".
fn static_value(field: *mut FieldInfo) -> Option<String> {
    if il2cpp_field_get_flags(field) & FIELD_ATTRIBUTE_STATIC == 0 {
        return None;
    }

    // A `const` has no slot in the static data area, so reading through its FieldInfo
    // would report unrelated memory as if it were this field's value.
    if il2cpp_field_get_flags(field) & FIELD_ATTRIBUTE_LITERAL != 0 || il2cpp_field_is_literal(field) {
        return None;
    }

    let field_type = il2cpp_field_get_type(field);
    if field_type.is_null() {
        return None;
    }

    match unsafe { (*field_type).type_() } {
        Il2CppTypeEnum_IL2CPP_TYPE_R4 => {
            let mut value: f32 = 0.0;
            il2cpp_field_static_get_value(field, &mut value as *mut f32 as *mut c_void);
            Some(format!("{value}"))
        }
        Il2CppTypeEnum_IL2CPP_TYPE_R8 => {
            let mut value: f64 = 0.0;
            il2cpp_field_static_get_value(field, &mut value as *mut f64 as *mut c_void);
            Some(format!("{value}"))
        }
        Il2CppTypeEnum_IL2CPP_TYPE_I4 => {
            let mut value: i32 = 0;
            il2cpp_field_static_get_value(field, &mut value as *mut i32 as *mut c_void);
            Some(format!("{value}"))
        }
        Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN => {
            let mut value: u8 = 0;
            il2cpp_field_static_get_value(field, &mut value as *mut u8 as *mut c_void);
            Some(format!("{}", value != 0))
        }
        _ => None
    }
}

fn field_label(field: *mut FieldInfo) -> String {
    let value = match static_value(field) {
        Some(value) => format!(" = {value}"),
        None => String::new()
    };

    format!("[{}]{}", field_signature(field), value)
}

struct Counts {
    classes: usize,
    methods: usize,
    fields: usize,
    // Full dumps granted from the allowlist budget, reported apart from the general ones.
    allowlisted: usize,
    // A8: matching classes the general cap passed over. Counted so the log can say which of
    // its gaps are the cap and which are the client.
    skipped_full: usize
}

/// The lines a finished dump ends with: the counts line a run reads out of it, the cap note
/// when the 500 class cap was spent, and the completion marker a later launch looks for.
/// `dump_if_enabled` writes these and the tests build a dump from them, so both read one
/// definition of the frame.
fn tail_lines(counts: &Counts) -> Vec<String> {
    let mut lines = vec![
        format!(
            "\n{} classes ({} from the allowlist), {} methods, {} fields written",
            counts.classes, counts.allowlisted, counts.methods, counts.fields
        )
    ];

    if let Some(note) = full_class_cap_note(counts) {
        lines.push(note);
    }

    lines.push(FOOTER_MARKER.to_owned());
    lines
}

/// The line that makes the 500 class truncation say so in the log instead of swallowing it
/// silently (A8). A class the cap passed over still shows up as method and field hits; this
/// is how a reader knows no block of its own was owed.
fn full_class_cap_note(counts: &Counts) -> Option<String> {
    if counts.skipped_full == 0 {
        return None;
    }

    let general = counts.classes - counts.allowlisted;

    Some(format!(
        "[full class cap {MAX_FULL_CLASSES} spent: {general} general dumps granted, {} more matching classes are named above but never dumped in full. Raise MAX_FULL_CLASSES to see their signatures.]",
        counts.skipped_full
    ))
}

fn dump_full_class<W: Write>(w: &mut W, klass: *mut Il2CppClass, label: &str, counts: &mut Counts) -> bool {
    counts.classes += 1;
    if writeln!(w, "\n=== {label} ===").is_err() {
        return false;
    }

    let mut iter: *mut c_void = ptr::null_mut();
    loop {
        let method = il2cpp_class_get_methods(klass, &mut iter);
        if method.is_null() {
            break;
        }

        let name = as_string(unsafe { (*method).name }).unwrap_or_default();
        let arity = unsafe { (*method).parameters_count };
        counts.methods += 1;

        if writeln!(w, "  {name}/{arity} -> {}", method_signature(method)).is_err() {
            return false;
        }
    }

    let mut iter: *mut c_void = ptr::null_mut();
    loop {
        let field = il2cpp_class_get_fields(klass, &mut iter);
        if field.is_null() {
            break;
        }

        let name = as_string(unsafe { (*field).name }).unwrap_or_default();
        counts.fields += 1;

        if writeln!(w, "  field {name} {}", field_label(field)).is_err() {
            return false;
        }
    }

    true
}

fn scan_class<W: Write>(
    w: &mut W,
    klass: *mut Il2CppClass,
    label: &str,
    counts: &mut Counts,
    allowlisted: bool,
) -> bool {
    // An allowlisted class is dumped in full without the hit scan below: the full dump already
    // contains every method and field that scan could have matched, so scanning first would only
    // duplicate the work and the lines.
    if allowlisted && counts.allowlisted < MAX_ALLOWLIST_CLASSES {
        counts.allowlisted += 1;
        return dump_full_class(w, klass, label, counts);
    }
    let mut matched = false;

    let mut iter: *mut c_void = ptr::null_mut();
    loop {
        let method = il2cpp_class_get_methods(klass, &mut iter);
        if method.is_null() {
            break;
        }

        let name = match as_string(unsafe { (*method).name }) {
            Some(name) => name,
            None => continue
        };

        if !matches_any(&name, METHOD_FILTERS) {
            continue;
        }

        matched = true;
        counts.methods += 1;

        let arity = unsafe { (*method).parameters_count };
        if writeln!(w, "{label}::{name}/{arity} -> {}", method_signature(method)).is_err() {
            return false;
        }

        if counts.methods >= MAX_HITS {
            return false;
        }
    }

    let mut iter: *mut c_void = ptr::null_mut();
    loop {
        let field = il2cpp_class_get_fields(klass, &mut iter);
        if field.is_null() {
            break;
        }

        let name = match as_string(unsafe { (*field).name }) {
            Some(name) => name,
            None => continue
        };

        if !matches_any(&name, FIELD_FILTERS) {
            continue;
        }

        matched = true;
        counts.fields += 1;

        if writeln!(w, "{label}::field {name} {}", field_label(field)).is_err() {
            return false;
        }

        if counts.fields >= MAX_HITS {
            return false;
        }
    }

    // A class that is interesting by name gets the full treatment on top of the hits
    // above, so the whole surface around a transition is visible at once.
    if matched || matches_any(label, CLASS_FILTERS) {
        if counts.classes < MAX_FULL_CLASSES {
            return dump_full_class(w, klass, label, counts);
        }

        // Past the cap the walk carries on and the hits above carry on, so the class is not
        // lost; what is lost is its full surface, and that is now counted (A8).
        counts.skipped_full += 1;
    }

    true
}

fn dump_inner<W: Write>(w: &mut W, scope: &Scope) -> Counts {
    let mut counts = Counts { classes: 0, methods: 0, fields: 0, allowlisted: 0, skipped_full: 0 };

    if let Some(reason) = scope.missing {
        let _ = writeln!(w, "{reason}");
        return counts;
    }

    for scope_image in &scope.images {
        let _ = writeln!(w, "\n########## {} ##########", scope_image.name);

        for j in 0..scope_image.class_count {
            let klass = il2cpp_image_get_class(scope_image.image, j);
            if klass.is_null() {
                continue;
            }

            let name = as_string(il2cpp_class_get_name(klass as *mut Il2CppClass)).unwrap_or_default();
            let namespace = as_string(il2cpp_class_get_namespace(klass as *mut Il2CppClass)).unwrap_or_default();
            let label = if namespace.is_empty() { name.clone() } else { format!("{namespace}.{name}") };

            let allowlisted = FULL_DUMP_NAMES.iter().any(|allowed| allowed.eq_ignore_ascii_case(&name));

            if !scan_class(w, klass as *mut Il2CppClass, &label, &mut counts, allowlisted) {
                let _ = writeln!(w, "\n[truncated at {MAX_HITS} matches]");
                return counts;
            }
        }
    }

    counts
}

/// Write `<data dir>/introspect.log` when `debug_mode` is on and no current dump of this
/// client's metadata is on disk yet. When one is, reuse it: C44.
pub fn dump_if_enabled() {
    let hachimi = Hachimi::instance();
    if !hachimi.config.load().debug_mode {
        return;
    }

    let path = hachimi.get_data_path("introspect.log");

    // The scope pass is the whole cost of a launch that can reuse the dump: image headers,
    // no class walk, no formatting, nothing written.
    let mut scope = Scope { images: Vec::new(), missing: Some("metadata stamp walk faulted") };
    let scope_ok = crate::core::utils::seh_guard("introspect metadata stamp", || {
        scope = collect_scope();
    });

    let stamp = if scope_ok { DumpStamp::of(&scope.images, &client_identity()) } else { None };

    if let Some(stamp) = &stamp {
        if let Some(size) = existing_dump_size_if_current(&path, stamp) {
            info!(
                "introspect: reusing {} ({} bytes) written for stamp {:016x}; delete the file to re-dump",
                path.display(),
                size,
                stamp.hash
            );
            return;
        }
    }

    if let Some(parent) = path.parent() {
        let _ = create_dir_all(parent);
    }

    let file = match File::create(&path) {
        Ok(file) => file,
        Err(e) => {
            error!("introspect: could not write {}: {}", path.display(), e);
            return;
        }
    };

    let mut writer = BufWriter::new(file);

    // The stamp first: a reader, and the next launch, have to be able to tell which client
    // and which rules this dump came from. Written only when the scope read worked, so a
    // log with no stamp line is never treated as current.
    if let Some(stamp) = &stamp {
        let _ = writeln!(writer, "{}", stamp.header_line());
    }

    let guard_ok = crate::core::utils::seh_guard("introspect", || {
        let counts = dump_inner(&mut writer, &scope);

        // The tail, and the completion marker last: a walk that faulted never reaches it, so
        // the file it left behind is not mistaken for a complete dump.
        for line in tail_lines(&counts) {
            let _ = writeln!(writer, "{line}");
        }
    });

    if !guard_ok {
        error!("introspect: metadata walk faulted, the log is partial");
    }

    let _ = writer.flush();

    // The same stamp a later launch looks for, so a run can tell a fresh dump from a reuse
    // without opening the file.
    match &stamp {
        Some(stamp) => info!("introspect: wrote {} for stamp {:016x}", path.display(), stamp.hash),
        None => info!("introspect: wrote {} with no stamp, the metadata scope did not read", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::io::Cursor;

    // A Global client scope in shape and not in numbers: the six in-scope images the captured
    // dump walked, with stand-in class counts. The stamp reads a name and a count per image,
    // and `DumpStamp::of` never touches the image pointer, so null images are fine here.
    fn scope_image(name: &str, class_count: usize) -> ScopeImage {
        ScopeImage { image: ptr::null(), name: name.to_owned(), class_count }
    }

    fn global_client_scope() -> Vec<ScopeImage> {
        vec![
            scope_image("umamusume.dll", 27524),
            scope_image("umamusume.Http.dll", 41),
            scope_image("DOTween.dll", 320),
            scope_image("DOTweenPro.dll", 58),
            scope_image("cute_payment.dll", 12),
            scope_image("Assembly-CSharp.dll", 1900),
        ]
    }

    fn stamp_of(scope: &[ScopeImage]) -> DumpStamp {
        DumpStamp::of(scope, "UmamusumePersPrettyDerby.exe:23536640").expect("a scope with images stamps")
    }

    // The counts the captured 1,966,611 byte dump ended with, and the 1,253 distinct classes
    // that are in it as filter hits with no full block of their own.
    fn captured_counts() -> Counts {
        Counts { classes: 534, allowlisted: 34, methods: 15772, fields: 11455, skipped_full: 1253 }
    }

    /// A dump the way this build frames one: the stamp line, a stand-in for the body
    /// `dump_inner` writes, and the tail from `tail_lines`.
    fn a_dump_written_by_this_build(stamp: &DumpStamp, counts: &Counts, body_bytes: usize) -> Vec<u8> {
        let mut dump = stamp.header_line().into_bytes();
        dump.extend_from_slice(b"\n########## umamusume.dll ##########\n");
        dump.extend_from_slice(&vec![b'x'; body_bytes]);

        for line in tail_lines(counts) {
            dump.extend_from_slice(line.as_bytes());
            dump.push(b'\n');
        }

        dump
    }

    struct CountingReader {
        inner: Cursor<Vec<u8>>,
        bytes_read: Cell<u64>,
    }

    impl CountingReader {
        fn new(data: Vec<u8>) -> Self {
            CountingReader { inner: Cursor::new(data), bytes_read: Cell::new(0) }
        }
    }

    impl Read for CountingReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let read = self.inner.read(buf)?;
            self.bytes_read.set(self.bytes_read.get() + read as u64);
            Ok(read)
        }
    }

    impl Seek for CountingReader {
        fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    #[test]
    fn the_dump_one_launch_writes_is_the_one_the_next_launch_reuses() {
        let scope = global_client_scope();
        let stamp = stamp_of(&scope);
        let counts = captured_counts();
        let dump = a_dump_written_by_this_build(&stamp, &counts, 2_000_000);

        let mut reader = CountingReader::new(dump.clone());
        let (size, head, tail) = head_and_tail(&mut reader).expect("the two ends of a dump");

        assert_eq!(size, dump.len() as u64, "the reuse check reported the wrong size");
        assert!(
            dump_is_current(&head, &tail, &stamp),
            "a complete dump of this metadata and these rules was not reused:\nhead: {head}\ntail: {tail}"
        );
    }

    #[test]
    fn a_dump_on_disk_is_what_the_next_launch_reuses() {
        // The same decision `dump_if_enabled` makes against a real file, through the real
        // reader. What this cannot reach is the scope: a launch reads it from the game.
        // The scratch file lives under `target`, the one directory here that is gitignored and
        // scratch by right.
        let scratch = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/tmp");
        std::fs::create_dir_all(&scratch).expect("the scratch directory is there");
        let path = scratch.join(format!("introspect-reuse-{}.log", std::process::id()));
        let scope = global_client_scope();
        let stamp = stamp_of(&scope);
        let dump = a_dump_written_by_this_build(&stamp, &captured_counts(), 2_000_000);

        std::fs::write(&path, &dump).expect("the dump lands on disk");

        assert_eq!(
            existing_dump_size_if_current(&path, &stamp),
            Some(dump.len() as u64),
            "a launch did not reuse the dump already on disk"
        );

        // A client update moves the stamp: the file stays, and the next launch rewrites it.
        let mut moved = global_client_scope();
        moved[0].class_count += 1;
        assert_eq!(existing_dump_size_if_current(&path, &stamp_of(&moved)), None, "a stale dump was reused");

        assert_eq!(existing_dump_size_if_current(&path.with_extension("absent"), &stamp), None, "a launch reused a file that is not there");

        std::fs::remove_file(&path).expect("the scratch file is removed");
    }

    #[test]
    fn a_reuse_check_never_reads_the_body_it_is_reusing() {
        let scope = global_client_scope();
        let stamp = stamp_of(&scope);
        let dump = a_dump_written_by_this_build(&stamp, &captured_counts(), 2_000_000);
        let bytes = dump.len() as u64;

        let mut reader = CountingReader::new(dump);
        let (size, head, tail) = head_and_tail(&mut reader).expect("the two ends of a dump");

        assert_eq!(size, bytes);
        assert!(head.starts_with("# introspect dump v"), "the head read is not the stamp line: {head}");
        assert!(tail.contains(FOOTER_MARKER), "the tail read is not the completion line: {tail}");
        assert_eq!(
            reader.bytes_read.get(),
            HEAD_BYTES + TAIL_BYTES,
            "a {} byte dump took {} bytes of read to decide whether to reuse it",
            bytes,
            reader.bytes_read.get()
        );
    }

    #[test]
    fn a_dump_of_the_same_client_and_the_same_rules_stamps_the_same_line_twice() {
        // The write once and reuse half of C44: nothing about a second launch moves the line.
        let first = stamp_of(&global_client_scope());
        let second = stamp_of(&global_client_scope());
        assert_eq!(first.header_line(), second.header_line());
        assert_eq!(first.hash, second.hash);
    }

    #[test]
    fn a_client_or_rule_the_dump_came_from_moving_drops_the_reuse() {
        let current = stamp_of(&global_client_scope()).header_line();

        let mut renamed = global_client_scope();
        renamed[0].name = "umamusume_renamed.dll".to_owned();
        assert_ne!(stamp_of(&renamed).header_line(), current, "a renamed in-scope image was not noticed");

        let mut more_classes = global_client_scope();
        more_classes[0].class_count += 1;
        assert_ne!(stamp_of(&more_classes).header_line(), current, "a class count moving was not noticed");

        let mut extra_image = global_client_scope();
        extra_image.push(scope_image("cyspring.dll", 30));
        assert_ne!(stamp_of(&extra_image).header_line(), current, "an image coming into scope was not noticed");

        let other_client = DumpStamp::of(&global_client_scope(), "UmamusumePersPrettyDerby.exe:23540736");
        assert_ne!(other_client.expect("stamps").header_line(), current, "a different client install was not noticed");

        assert!(DumpStamp::of(&[], "unknown").is_none(), "an empty scope stamped something");
    }

    #[test]
    fn a_dump_cut_short_by_a_fault_is_never_reused() {
        let scope = global_client_scope();
        let stamp = stamp_of(&scope);

        // The shape `seh_guard` leaves behind: the walk faulted, so `tail_lines` - and with it
        // the completion marker - was never written.
        let mut partial = stamp.header_line().into_bytes();
        partial.extend_from_slice(b"\n########## umamusume.dll ##########\n<Gallop.NowLoading::PlayFadeNowLoading/3 -> void()\n");

        let mut reader = Cursor::new(partial);
        let (_, head, tail) = head_and_tail(&mut reader).expect("the two ends of a partial dump");

        assert!(!dump_is_current(&head, &tail, &stamp), "a partial dump was taken for a complete one:\ntail: {tail}");
    }

    #[test]
    fn the_log_the_previous_builds_wrote_is_not_taken_for_a_current_one() {
        // The head and the tail of `run log\introspect.log`, 1,966,611 bytes: what runs 11 to
        // 13 wrote, unstamped and with the counts line last. That file costs a launch to write
        // again, which is C44, and this is the check that makes the first launch after the fix
        // re-dump instead of trusting it.
        let head = "\n########## umamusume.dll ##########\n<CoroutineGaugeUpAnimation>d__21::field firstDelay [public float]\n\n=== <CoroutineGaugeUpAnimation>d__21 ===\n  .ctor/1 -> void(int)\n  MoveNext/0 -> bool()\n";
        let tail = "\n########## cute_payment.dll ##########\n\n########## Assembly-CSharp.dll ##########\n\n534 classes (34 from the allowlist), 15772 methods, 11455 fields written";

        let scope = global_client_scope();
        let stamp = stamp_of(&scope);
        assert!(!dump_is_current(head, tail, &stamp), "an unstamped dump was reused");
    }

    #[test]
    fn a_small_dump_reuses_too() {
        // A client whose in-scope metadata is small: the head and tail buffers overlap in the
        // file, and the check still has to find both ends.
        let scope = vec![scope_image("umamusume.dll", 3)];
        let stamp = stamp_of(&scope);
        let counts = Counts { classes: 3, allowlisted: 0, methods: 9, fields: 1, skipped_full: 0 };
        let dump = a_dump_written_by_this_build(&stamp, &counts, 40);

        assert!(dump.len() < (HEAD_BYTES + TAIL_BYTES) as usize, "the fixture stopped being small");

        let mut reader = Cursor::new(dump);
        let (_, head, tail) = head_and_tail(&mut reader).expect("the two ends of a small dump");
        assert!(dump_is_current(&head, &tail, &stamp), "a complete small dump was not reused:\nhead: {head}\ntail: {tail}");
    }

    #[test]
    fn the_stamp_line_fits_inside_the_head_a_launch_reads() {
        let stamp = stamp_of(&global_client_scope());
        let line = stamp.header_line();

        assert!(line.len() < HEAD_BYTES as usize, "the {} byte stamp line is wider than the {} byte head", line.len(), HEAD_BYTES);
    }

    #[test]
    fn the_full_class_cap_reports_itself() {
        // 534 blocks in all, 34 of them from the allowlist budget, so 500 general dumps spent
        // the cap, and 1,253 matching classes never got a block (A8).
        let spent = captured_counts();
        let note = full_class_cap_note(&spent).expect("a spent cap reports itself");

        assert!(note.contains("cap 500 spent"), "the note does not name the cap: {note}");
        assert!(note.contains("500 general dumps granted"), "the note does not say what was granted: {note}");
        assert!(note.contains("1253 more matching classes"), "the note does not count the gap: {note}");

        let unspent = Counts { classes: 3, allowlisted: 0, methods: 9, fields: 1, skipped_full: 0 };
        assert!(full_class_cap_note(&unspent).is_none(), "an unspent cap claimed it was reached");
    }

    #[test]
    fn the_tail_keeps_the_counts_line_a_run_reads_and_closes_with_the_marker() {
        let lines = tail_lines(&captured_counts());

        assert_eq!(lines[0], "\n534 classes (34 from the allowlist), 15772 methods, 11455 fields written");
        assert_eq!(lines.last().map(String::as_str), Some(FOOTER_MARKER));
    }

    /// The C44 byte measurement, run by hand against a captured dump: `HACHIMI_INTROSPECT_CAPTURE`
    /// names the file, and nothing runs it automatically because the captures are gitignored.
    /// It reads the capture's two ends through the reuse check, rebuilds the same client dump
    /// between the frame lines this build writes, and prints what a launch pays either way.
    #[test]
    #[ignore]
    fn measure_the_dump_a_launch_writes() {
        let path = match std::env::var("HACHIMI_INTROSPECT_CAPTURE") {
            Ok(path) => Path::new(&path).to_path_buf(),
            Err(_) => {
                println!("set HACHIMI_INTROSPECT_CAPTURE to a captured introspect.log to run this");
                return;
            }
        };

        let text = std::fs::read_to_string(&path).expect("the capture is readable and utf-8");
        let before = text.len() as u64;

        // What the capture says it wrote, read off its own counts line.
        let counts_line = text.lines().last().unwrap_or_default().to_owned();
        let numbers: Vec<usize> = counts_line
            .split(|c: char| !c.is_ascii_digit())
            .filter(|part| !part.is_empty())
            .map(|part| part.parse::<usize>().expect("a number out of the counts line"))
            .collect();

        let mut images: Vec<ScopeImage> = Vec::new();
        let mut blocks = 0usize;
        let mut hit_labels: Vec<&str> = Vec::new();
        let mut block_labels: Vec<&str> = Vec::new();

        for line in text.lines() {
            if let Some(name) = line.strip_prefix("########## ").and_then(|rest| rest.strip_suffix(" ##########")) {
                // A log does not carry the class counts a live domain reports. The stamp needs
                // one number per image, so the frame length printed here is the measurement and
                // the hash digits are the stand-in.
                images.push(scope_image(name, 1000));
            }
            else if let Some(label) = line.strip_prefix("=== ").and_then(|rest| rest.strip_suffix(" ===")) {
                blocks += 1;
                block_labels.push(label);
            }
            else if line.contains("::") {
                let label = &line[..line.find("::").unwrap_or(0)];
                if !label.is_empty() && !hit_labels.contains(&label) {
                    hit_labels.push(label);
                }
            }
        }

        // A lower bound on the classes the 500 cap passed over: every class the filters hit
        // that has no full block of its own.
        let capped = hit_labels.iter().filter(|label| !block_labels.contains(label)).count();

        assert_eq!(numbers.len(), 4, "the capture does not end with a counts line: {counts_line}");
        assert_eq!(numbers[0], blocks, "the capture holds {blocks} full blocks, its counts line says {}", numbers[0]);

        let counts = Counts {
            classes: numbers[0],
            allowlisted: numbers[1],
            methods: numbers[2],
            fields: numbers[3],
            skipped_full: capped,
        };

        let mut file = File::open(&path).expect("the capture is readable");
        let (size, head, tail) = head_and_tail(&mut file).expect("the two ends of the capture");

        println!("capture             : {} ({} bytes, {} images in scope, {} full blocks, {} classes hit but never dumped)",
                 path.display(), size, images.len(), blocks, capped);
        println!("reuse verdict today   : {} (no stamp line at the head, no completion line at the tail)",
                 dump_is_current(&head, &tail, &stamp_of(&images)));

        let stamp = stamp_of(&images);
        let header = stamp.header_line();
        let note = full_class_cap_note(&counts).unwrap_or_default();
        let tail = tail_lines(&counts);
        let frame = header.len() + 1 + tail.iter().map(|line| line.len() + 1).sum::<usize>();

        println!("stamp line          : {header}");
        println!("cap note            : {note}");
        println!("frame this build adds : {frame} bytes");

        // The same client dump, put between those frame lines.
        let body = match text.rfind(counts_line.as_str()) {
            Some(at) => &text[..at],
            None => &text[..]
        };

        let mut rebuilt = header.into_bytes();
        rebuilt.extend_from_slice(body.as_bytes());
        for line in tail {
            rebuilt.extend_from_slice(line.as_bytes());
            rebuilt.push(b'\n');
        }

        let mut reader = Cursor::new(rebuilt);
        let (after, head, tail) = head_and_tail(&mut reader).expect("the two ends of the rebuilt dump");

        println!("same dump, framed     : {after} bytes, reused by the next launch: {}", dump_is_current(&head, &tail, &stamp));
        println!("written per launch    : before {before}, first launch for this client {after}, every later launch 0");
    }
}
