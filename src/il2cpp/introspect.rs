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

use std::ffi::{CStr, c_char};
use std::fs::{File, create_dir_all};
use std::io::{BufWriter, Write};
use std::os::raw::c_void;
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
    "elapsedtime", "currenttime", "settime", "curspeed", "interval"
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
const MAX_FULL_CLASSES: usize = 500;

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
    fields: usize
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

fn scan_class<W: Write>(w: &mut W, klass: *mut Il2CppClass, label: &str, counts: &mut Counts) -> bool {
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
    if (matched || matches_any(label, CLASS_FILTERS)) && counts.classes < MAX_FULL_CLASSES {
        return dump_full_class(w, klass, label, counts);
    }

    true
}

fn dump_inner<W: Write>(w: &mut W) -> Counts {
    let mut counts = Counts { classes: 0, methods: 0, fields: 0 };

    let domain = il2cpp_domain_get();
    if domain.is_null() {
        let _ = writeln!(w, "no IL2CPP domain");
        return counts;
    }

    let mut size: usize = 0;
    let assemblies = il2cpp_domain_get_assemblies(domain as *const Il2CppDomain, &mut size);
    if assemblies.is_null() {
        let _ = writeln!(w, "no assemblies");
        return counts;
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

        let image_name = as_string(il2cpp_image_get_name(image)).unwrap_or_default();
        if !image_is_in_scope(&image_name) {
            continue;
        }

        let _ = writeln!(w, "\n########## {image_name} ##########");

        let class_count = il2cpp_image_get_class_count(image);
        for j in 0..class_count {
            let klass = il2cpp_image_get_class(image, j);
            if klass.is_null() {
                continue;
            }

            let name = as_string(il2cpp_class_get_name(klass as *mut Il2CppClass)).unwrap_or_default();
            let namespace = as_string(il2cpp_class_get_namespace(klass as *mut Il2CppClass)).unwrap_or_default();
            let label = if namespace.is_empty() { name.clone() } else { format!("{namespace}.{name}") };

            if !scan_class(w, klass as *mut Il2CppClass, &label, &mut counts) {
                let _ = writeln!(w, "\n[truncated at {MAX_HITS} matches]");
                return counts;
            }
        }
    }

    counts
}

/// Write `<data dir>/introspect.log` when `debug_mode` is on, otherwise do nothing.
pub fn dump_if_enabled() {
    let hachimi = Hachimi::instance();
    if !hachimi.config.load().debug_mode {
        return;
    }

    let path = hachimi.get_data_path("introspect.log");
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
    let guard_ok = crate::core::utils::seh_guard("introspect", || {
        let counts = dump_inner(&mut writer);
        let _ = writeln!(
            writer,
            "\n{} classes, {} methods, {} fields written",
            counts.classes, counts.methods, counts.fields
        );
    });

    if !guard_ok {
        error!("introspect: metadata walk faulted, the log is partial");
    }

    let _ = writer.flush();
    info!("introspect: wrote {}", path.display());
}
