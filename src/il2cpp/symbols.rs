use std::borrow::Cow;
use std::collections::hash_map;
use std::ffi::CStr;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::os::raw::c_void;
use std::sync::Mutex;

use fnv::{FnvHashMap, FnvHashSet};
use once_cell::sync::Lazy;

use crate::core::hachimi::recover_lock;
use crate::core::Hachimi;
use crate::symbols_impl;
use crate::core::Error;

use super::api::*;
use super::ext::Il2CppObjectExt;
use super::types::*;
use super::types::Il2CppClass;
use std::ptr::null_mut;

static mut HANDLE: *mut c_void = null_mut();
static mut DOMAIN: *mut Il2CppDomain = null_mut();

pub unsafe fn dlsym(name: &str) -> usize {
    // The Global Android build ships a hollowed libil2cpp.so: 2347 of its 2388 dynamic
    // symbol entries are zeroed and no il2cpp_* name exists anywhere in the file, so the
    // platform dlsym may not find the API. libunity carries an equivalent C-API table that
    // it fills with its own dlsym calls; we read that table in-process (see slot_table).
    // Whether dlsym works on a given device/build is measured once by slot_table::diagnostic.
    #[cfg(target_os = "android")]
    {
        let addr = super::slot_table::resolve(name);
        if addr != 0 {
            return addr;
        }
    }

    symbols_impl::dlsym(HANDLE, name)
}

pub fn set_handle(handle: usize) {
    unsafe { HANDLE = handle as *mut c_void }
}

/// Re-measure once the game has finished loading its il2cpp image. A protection layer
/// that rebuilds the symbol table itself only becomes visible by this point. This is the
/// one full sweep the process performs; the state at `dlopen` time is carried by the
/// cached validation verdict in `slot_table::table_base`.
pub fn recheck() {
    #[cfg(target_os = "android")]
    unsafe { super::slot_table::diagnostic(HANDLE as usize, "post-load") }
}

pub fn init() {
    unsafe { DOMAIN = il2cpp_domain_get() }
}

pub fn get_assembly_image(assembly_name: &CStr) -> Result<*const Il2CppImage, Error> {
    let domain = unsafe { DOMAIN };

    // C9: `DOMAIN` is whatever `il2cpp_domain_get` handed back at init. Asked before the game has
    // a domain, this would walk the game's domain table from address 0; "assembly not found" is
    // what every caller of this already handles.
    if domain.is_null() {
        return Err(Error::AssemblyNotFound(assembly_name.to_str().unwrap().to_owned()));
    }

    let assembly = il2cpp_domain_assembly_open(domain, assembly_name.as_ptr());
    if assembly.is_null() {
        Err(Error::AssemblyNotFound(assembly_name.to_str().unwrap().to_owned()))
    }
    else {
        Ok(il2cpp_assembly_get_image(assembly))
    }
}

pub fn get_class(image: *const Il2CppImage, namespace: &CStr, class_name: &CStr) -> Result<*mut Il2CppClass, Error> {
    let class = il2cpp_class_from_name(image, namespace.as_ptr(), class_name.as_ptr());
    if class.is_null() {
        Err(Error::ClassNotFound(namespace.to_str().unwrap().to_owned(), class_name.to_str().unwrap().to_owned()))
    }
    else {
        Ok(class)
    }
}

pub fn get_method(class: *mut Il2CppClass, name: &CStr, args_count: i32) -> Result<*const MethodInfo, Error> {
    let method = il2cpp_class_get_method_from_name(class, name.as_ptr(), args_count);
    if method.is_null() {
        Err(Error::MethodNotFound(name.to_str().unwrap().to_owned()))
    }
    else {
        // Barrier item 2 (C2): a door armed off the name plus argument list answer (C7) stands on the
        // same method a signature walk would have found, so the record is written from the method this
        // lookup named. It is the method that decides how its hook comes down, not the spelling used to
        // look it up. A name that is not `MoveNext` never becomes a door, and a `MoveNext` whose
        // signature is not the instance `() -> bool` is refused by the rule, not by the request.
        if args_count == 0 && name == c"MoveNext" {
            record_door_from_method(method, 0);
        }

        Ok(method)
    }
}

// Whether one dumped parameter answers what a wrapper declared. A generic instantiation such as
// `List<SupportCardData>` reports `GENERICINST` in its parameter record, not `CLASS`, so the exact
// walk refuses a method that really exists (C48). A caller whose wrapper declares a pointer in that
// slot may ask for the wider answer, because a generic instantiation of a class is a managed object
// and the ABI moves it as an address.
pub fn param_type_accepts(requested: Il2CppTypeEnum, actual: Il2CppTypeEnum, generic_slots: bool) -> bool {
    if actual == requested {
        return true;
    }

    generic_slots
        && actual == Il2CppTypeEnum_IL2CPP_TYPE_GENERICINST
        && matches!(requested, Il2CppTypeEnum_IL2CPP_TYPE_CLASS | Il2CppTypeEnum_IL2CPP_TYPE_OBJECT)
}

// `MethodAttributes::Static`. A wrapper that reserves the first register for `this` cannot take a
// static target, because that register holds the target's first real argument, and a wrapper that
// declares only the real arguments cannot take an instance one.
pub const METHOD_ATTRIBUTE_STATIC: u16 = 0x0010;

/// What a hook site asks a class for: the parameters its wrapper declares, the return type that
/// wrapper reads, whether it reserves a register for `this`, and whether a slot it declared as a
/// pointer may be answered by a generic instantiation (C48).
///
/// The signature is one value that travels through the class table. It used to be split: the walk
/// matched the name and the parameter list, and the caller checked the return type and the static
/// bit afterwards against whichever method the walk had already picked. On a pair like
/// `Gallop.ModelController::GetBodyShader/2` - this client's dump lists the `static` one at
/// `introspect.log:1188` and the instance one at 1189, under one name, one argument count and the
/// same two parameter enums - that order answers candidate 0, the caller's check then refuses it,
/// and the half the wrapper was written for is never reached. Nothing in this tree asks for that
/// name today, so no install here has failed that way: the pair is latent, and the static bit the
/// request now carries is what keeps it latent (the count of such pairs this client actually has,
/// and which of them a hook site names, is the scan written up in the C15 concept).
#[derive(Clone, Copy, Debug)]
pub struct MethodRequest<'a> {
    pub params: &'a [Il2CppTypeEnum],
    /// `None` for the name plus argument list answer the sites outside `AnimationSpeed` still ask
    /// for (C7): a return type that was never asked for cannot refuse a candidate.
    pub ret: Option<Il2CppTypeEnum>,
    pub allow_static: bool,
    pub require_static: bool,
    pub generic_slots: bool,
}

/// One method the class table offered under a name and a parameter list, carrying what the game
/// declares about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverloadCandidate {
    pub method: *const MethodInfo,
    /// The method's own return type, or `None` when the metadata left the record unfilled. A type
    /// this client cannot read is not a signature to bind (C9).
    pub ret: Option<Il2CppTypeEnum>,
    pub is_static: bool,
}

/// Why one candidate does not carry the request. The reason travels with the refusal so an install
/// line can say which method it looked at and what that method answered, instead of only that
/// nothing installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverloadRejection {
    Return { actual: Option<Il2CppTypeEnum>, expected: Il2CppTypeEnum },
    Staticness { candidate_is_static: bool },
}

/// The pure half of the resolution: given the signature a caller asked for, does this candidate
/// carry it? Kept free of il2cpp so it is testable the way `param_type_accepts` is;
/// `get_method_overloads` is the half that reads the class table.
pub fn overload_rejects_request(candidate: &OverloadCandidate, request: &MethodRequest) -> Option<OverloadRejection> {
    if let Some(expected) = request.ret {
        if candidate.ret != Some(expected) {
            return Some(OverloadRejection::Return { actual: candidate.ret, expected });
        }
    }

    if candidate.is_static && !request.allow_static {
        return Some(OverloadRejection::Staticness { candidate_is_static: true });
    }

    if !candidate.is_static && request.require_static {
        return Some(OverloadRejection::Staticness { candidate_is_static: false });
    }

    None
}

/// Every method the class table offers under a name and a parameter list, each carrying the return
/// type and the static bit the game declares for it. The walk does not stop at the first name match
/// any more: the answer to a signature is every method that carries it, and picking one of them by
/// the table's order is the class-level decision C15 is about.
pub fn get_method_overloads(class: *mut Il2CppClass, name: &str, request: &MethodRequest) -> Vec<OverloadCandidate> {
    if !request.generic_slots {
        return walk_method_overloads(class, name, request);
    }

    // The wider walk is the second look and not the first: an overload a hook resolved against an
    // exact parameter list keeps resolving to that method (C48).
    let exact = walk_method_overloads(class, name, &MethodRequest { generic_slots: false, ..*request });

    if !exact.is_empty() {
        return exact;
    }

    walk_method_overloads(class, name, &MethodRequest { generic_slots: true, ..*request })
}

fn walk_method_overloads(class: *mut Il2CppClass, name: &str, request: &MethodRequest) -> Vec<OverloadCandidate> {
    let mut found: Vec<OverloadCandidate> = Vec::new();

    // C9: a class the hook site did not resolve is null, and asking the game for a method of it
    // walks the game's metadata from address 0.
    if class.is_null() {
        return found;
    }

    let mut iter: *mut c_void = null_mut();

    loop {
        let method = il2cpp_class_get_methods(class, &mut iter);
        if method.is_null() {
            break;
        }

        // Check name. The name is the game's, and a name this client cannot read is not a name to
        // compare, so the candidate is skipped rather than unwrapped (C2).
        let method_name = match unsafe { CStr::from_ptr((*method).name) }.to_str() {
            Ok(method_name) => method_name,
            Err(_) => continue,
        };

        if method_name != name {
            continue;
        }

        // Check params
        let param_count = il2cpp_method_get_param_count(method);
        if param_count != request.params.len() as u32 {
            continue;
        }

        let mut ok = true;
        for i in 0u32..param_count {
            let param = il2cpp_method_get_param(method, i);

            // C9: the parameter record is the game's. A slot it did not fill is not a type to
            // compare, so this overload is refused rather than read from address 0.
            if param.is_null() {
                ok = false;
                break;
            }

            if !param_type_accepts(request.params[i as usize], unsafe { (*param).type_() }, request.generic_slots) {
                ok = false;
                break;
            }
        }

        if !ok {
            continue;
        }

        let return_type = unsafe { (*method).return_type };
        let is_static = unsafe { (*method).flags } & METHOD_ATTRIBUTE_STATIC != 0;
        let ret = if return_type.is_null() { None } else { Some(unsafe { (*return_type).type_() }) };

        // Barrier item 2 (C2): a method the class table declares as the door a coroutine wrapper stands
        // on is recorded here, from its own signature, whether or not this caller ends up installing on
        // it - `select_overload` may still refuse an ambiguous pair, and a different hook site may be the
        // one that arms it. `Interceptor::unhook` reads the record to decide whether that take-down's
        // backend half waits for the game tick, and the only thing that decides it is the method.
        record_coroutine_door_if_the_signature_says_so(method_name, param_count, ret, is_static, unsafe { (*method).methodPointer });

        found.push(OverloadCandidate {
            method,
            ret,
            is_static,
        });
    }

    found
}

/// How many candidates carry the request.
///
/// `Ambiguous` is not a failure to hide: `CLASS` matches every reference type, so
/// `SingleModeResultContentBase::FadeInContentFromRight/3` taking `UnityEngine.CanvasGroup`
/// (`introspect.log:24167`) and the one taking `UnityEngine.UI.MaskableGraphic` (24168) are one
/// request to a matcher that compares enums. The caller reports the collision instead of letting
/// the order of the class table settle it in silence. Telling those two apart is parameter class
/// names, which is item A2 and not this decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverloadAnswer {
    None,
    Unique(usize),
    Ambiguous(usize),
}

/// The pure half of the resolution: which candidates carry the signature the request asks for.
/// Kept free of il2cpp and of allocation so the decision a hook install makes is testable the way
/// `param_type_accepts` is; `get_method_overloads` is the half that reads the class table.
pub fn select_overload(candidates: &[OverloadCandidate], request: &MethodRequest) -> OverloadAnswer {
    let mut count = 0usize;
    let mut first = usize::MAX;

    for (index, candidate) in candidates.iter().enumerate() {
        if overload_rejects_request(candidate, request).is_some() {
            continue;
        }

        count += 1;

        if count == 1 {
            first = index;
        }
    }

    match count {
        0 => OverloadAnswer::None,
        1 => OverloadAnswer::Unique(first),
        many => OverloadAnswer::Ambiguous(many),
    }
}

fn find_method_overload(class: *mut Il2CppClass, name: &str, params: &[Il2CppTypeEnum], generic_slots: bool) -> Result<*const MethodInfo, Error> {
    // The name plus argument list answer, unchanged for the sites that still ask for it (C7): the
    // first method the table offers under that name and parameter list, whatever it returns. The
    // signature-carrying half is `select_overload`, and `AnimationSpeed::resolve_method_any` is
    // what runs it.
    let request = MethodRequest { params, ret: None, allow_static: true, require_static: false, generic_slots };

    match get_method_overloads(class, name, &request).first() {
        Some(candidate) => Ok(candidate.method),
        None => Err(Error::MethodNotFound(name.to_owned())),
    }
}

pub fn get_method_overload(class: *mut Il2CppClass, name: &str, params: &[Il2CppTypeEnum]) -> Result<*const MethodInfo, Error> {
    find_method_overload(class, name, params, false)
}

pub fn get_method_addr(class: *mut Il2CppClass, name: &CStr, args_count: i32) -> usize {
    let res = get_method(class, name, args_count);
    if let Ok(method) = res {
        unsafe { (*method).methodPointer }
    }
    else {
        warn!("get_method_addr: {} = NULL", name.to_str().unwrap());
        0
    }
}

pub fn get_method_overload_addr(class: *mut Il2CppClass, name: &str, params: &[Il2CppTypeEnum]) -> usize {
    let res = get_method_overload(class, name, params);
    if let Ok(method) = res {
        unsafe { (*method).methodPointer }
    }
    else {
        warn!("get_method_overload_addr: {} = NULL", name);
        0
    }
}

pub static METHOD_CACHE: Lazy<
    Mutex<FnvHashMap<usize, FnvHashMap<(Cow<'_, CStr>, i32), usize>>>
> = Lazy::new(|| Mutex::default());

// Class -> the `MoveNext` address that survived the signature check, 0 when none did. Kept apart
// from `METHOD_CACHE` because the key here is a class alone: `MoveNext` is asked for by one
// signature, the one `MoveNextFn` declares, and keying a coroutine abort on a name plus an argument
// count is what C15 is about.
static MOVE_NEXT_CACHE: Lazy<Mutex<FnvHashMap<usize, usize>>> = Lazy::new(|| Mutex::default());

// C2: this cache is read from the plugin API's `extern "C"` entries, so a lock taken here
// must survive poisoning instead of turning a later plugin call into a panic across FFI.
pub fn get_method_cached(class: *mut Il2CppClass, name: &CStr, args_count: i32) -> Result<*const MethodInfo, Error> {
    let mut cache = recover_lock(&METHOD_CACHE);
    let entries = match cache.entry(class as usize) {
        hash_map::Entry::Occupied(e) => {
            if let Some(addr) = e.get().get(&(name.into(), args_count)) {
                if *addr == 0 {
                    // Only error that get_method returns
                    return Err(Error::MethodNotFound(name.to_str().unwrap().to_owned()));
                }
                else {
                    return Ok(*addr as *const MethodInfo);
                }
            }
            e.into_mut()
        },
        hash_map::Entry::Vacant(e) => e.insert(FnvHashMap::default())
    };
    let res = get_method(class, name, args_count);
    let addr = match res {
        Ok(addr) => addr as usize,
        Err(_) => 0
    };
    entries.insert((name.to_owned().into(), args_count), addr);
    res
}

pub fn get_method_addr_cached(class: *mut Il2CppClass, name: &CStr, args_count: i32) -> usize {
    let res = get_method_cached(class, name, args_count);
    if let Ok(method) = res {
        unsafe { (*method).methodPointer }
    }
    else {
        warn!("get_method_addr_cached: {} = NULL", name.to_str().unwrap());
        0
    }
}

pub fn find_nested_class(class: *mut Il2CppClass, name: &CStr) -> Result<*mut Il2CppClass, Error> {
    let mut iter: *mut c_void = null_mut();
    loop {
        let nested_class = il2cpp_class_get_nested_types(class, &mut iter);
        if nested_class.is_null() { break; }

        let class_name = unsafe { CStr::from_ptr((*nested_class).name) };
        if class_name == name {
            return Ok(nested_class);
        }
    }

    // C9: the parent class is the caller's, and on this branch it produced nothing. Naming it in
    // the error is fine for a real class; for a null one it is a read from address 0 on the way to
    // saying "not found", so the message says what is actually known.
    let class_name = if class.is_null() {
        "<unknown class>"
    }
    else {
        unsafe { CStr::from_ptr((*class).name) }.to_str().unwrap()
    };

    Err(Error::ClassNotFound(class_name.to_owned(), name.to_str().unwrap().to_owned()))
}

// A C# iterator or async method compiles to a class nested in the one that declares it, and a nested type
// is not reachable by the namespace and name lookup that finds every other class: this client stores the
// training cut's machine as Gallop.SingleModeMainTrainingCuttController/<PlayTrainingCut>d__70 while
// printing it under the bare name. The number in that name is the compiler's and moves between builds, so
// the walk matches a prefix and hands the name it landed on back for the log to show.
pub unsafe fn find_nested_class_by_prefix(class: *mut Il2CppClass, prefix: &str) -> Option<(String, *mut Il2CppClass)> {
    let mut iter: *mut c_void = null_mut();

    loop {
        let nested = il2cpp_class_get_nested_types(class, &mut iter);
        if nested.is_null() {
            return None;
        }

        let name = CStr::from_ptr((*nested).name);

        if let Ok(name) = name.to_str() {
            if name.starts_with(prefix) {
                return Some((name.to_owned(), nested));
            }
        }
    }
}

// C9: a field handle and the object it is read from come from two different places. The handle is
// this crate's own - null when the name did not resolve, which `get_field_from_name` already logs.
// The object is the game's, and null is an answer the game really gives: an unassigned reference
// field, a list slot nobody filled, a singleton that is not live yet. `il2cpp_field_get_value`
// turns either null into a read at (0 + offset) and `il2cpp_field_set_value` into a write there -
// an access violation on whatever thread the hook ran on. The refusal lives here, at the pair of
// helpers every `def_field_value_accessors!` / `def_field_object_accessors!` getter in the crate
// routes through, rather than at the several hundred places those getters are called.
pub fn get_field_value<T>(obj: *mut Il2CppObject, field: *mut FieldInfo) -> T {
    if obj.is_null() || field.is_null() {
        // "no object" and "no slot" read the same as "the slot holds zero", which is already the
        // answer these macros give for a handle that never resolved.
        return unsafe { MaybeUninit::zeroed().assume_init() };
    }

    let mut value = MaybeUninit::uninit();
    il2cpp_field_get_value(obj, field, unsafe { std::mem::transmute(&mut value) });
    unsafe { value.assume_init() }
}

pub fn get_field_object_value<T>(obj: *mut Il2CppObject, field: *mut FieldInfo) -> *mut T {
    get_field_value(obj, field)
}

/// Address of an instance field's slot, or null when there is no slot to read: a null `field` is a
/// name this crate never resolved, a null `obj` is the game saying there is nothing here, and a
/// caller that dereferenced either would be reading page 0 (C9). A reference slot holding null is
/// a different thing - that is the slot's own value, and `read_object` in
/// `hook/umamusume/CutStateProbe.rs` treats it as an answer rather than a failure.
pub fn get_field_ptr<T>(obj: *mut Il2CppObject, field: *mut FieldInfo) -> *mut T {
    if obj.is_null() || field.is_null() {
        return null_mut();
    }

    unsafe { (obj as usize + (*field).offset as usize) as _ }
}

pub fn set_field_value<T>(obj: *mut Il2CppObject, field: *mut FieldInfo, value: &T) {
    if obj.is_null() || field.is_null() {
        return;
    }

    il2cpp_field_set_value(obj, field, std::ptr::from_ref(value) as _);
}

pub fn set_field_object_value<T>(obj: *mut Il2CppObject, field: *mut FieldInfo, value: *const T) {
    // The same refusal as `set_field_value`, and the same argument shape the game's own setter
    // expects for a reference slot (`value` is what is stored, not where it is kept).
    if obj.is_null() || field.is_null() {
        return;
    }

    il2cpp_field_set_value(obj, field, value as _);
}

pub fn get_field_from_name(class: *mut Il2CppClass, name: &CStr) -> *mut FieldInfo {
    // C9: a class the hooks did not resolve is null, and asking the game for a field of it walks
    // the game's class metadata from address 0. A null handle is the answer the accessor macros
    // already give when a name has no field (C9).
    let field = if class.is_null() { null_mut() } else { il2cpp_class_get_field_from_name(class, name.as_ptr()) };

    if field.is_null() {
        warn!("get_field_from_name: {} = NULL", name.to_str().unwrap());
    }

    return field;
}

pub fn get_static_field_value<T: Default>(field: *mut FieldInfo) -> T {
    // C9: a static slot is storage the game owns; a handle that never resolved has no storage, and
    // the default is what every caller of a static getter already treats as "not set".
    if field.is_null() {
        return T::default();
    }

    let mut value = T::default();
    il2cpp_field_static_get_value(field, unsafe { std::mem::transmute(&mut value) });
    value
}

pub fn set_static_field_value<T>(field: *mut FieldInfo, value: T) {
    if field.is_null() {
        return;
    }

    il2cpp_field_static_set_value(field, std::ptr::from_ref(&value) as _);
}

pub fn get_static_field_object_value<T>(field: *mut FieldInfo) -> *mut T {
    if field.is_null() {
        return null_mut();
    }

    let mut value = null_mut();
    il2cpp_field_static_get_value(field, unsafe { std::mem::transmute(&mut value) });
    value
}

pub fn set_static_field_object_value<T>(field: *mut FieldInfo, value: *const T) {
    if field.is_null() {
        return;
    }

    il2cpp_field_static_set_value(field, value as _);
}

pub unsafe fn unbox<T: Copy>(obj: *mut Il2CppObject) -> T {
    // C9: a boxed value nobody filled is null, and the unbox of a null value answers null, so the
    // read would be at address 0. Zero is the same answer a caller gets for a value that was never
    // assigned (an enum slot, a nullable value type).
    if obj.is_null() {
        return unsafe { MaybeUninit::zeroed().assume_init() };
    }

    let data = il2cpp_object_unbox(obj);

    if data.is_null() {
        return unsafe { MaybeUninit::zeroed().assume_init() };
    }

    unsafe { *(data as *mut T) }
}

#[repr(transparent)]
pub struct IEnumerable<T = *mut Il2CppObject> {
    pub this: *mut Il2CppObject,
    _phantom: PhantomData<T>
}

impl<T> IEnumerable<T> {
    pub fn enumerator(&self) -> Option<IEnumerator> {
        if self.this.is_null() {
            return None;
        }

        let class = unsafe { (*self.this).klass() };
        let get_enumerator_addr = get_method_addr_cached(class, c"GetEnumerator", 0);
        if get_enumerator_addr == 0 {
            return None;
        }
        
        let get_enumerator: extern "C" fn(*mut Il2CppObject) -> *mut Il2CppObject = unsafe {
            std::mem::transmute(get_enumerator_addr)
        };

        Some(IEnumerator::from(get_enumerator(self.this)))
    }
}

impl<T> From<*mut Il2CppObject> for IEnumerable<T> {
    fn from(value: *mut Il2CppObject) -> Self {
        IEnumerable {
            this: value,
            _phantom: PhantomData
        }
    }
}

#[repr(transparent)]
pub struct IEnumerator<T = *mut Il2CppObject> {
    pub this: *mut Il2CppObject,
    _phantom: PhantomData<T>
}

pub type MoveNextFn = extern "C" fn(*mut Il2CppObject) -> bool;

// Barrier item 2 (C2) asks this file one question about a method before it decides how that method's
// hook comes down: is it the method a coroutine door stands on? `Interceptor::unhook` splits a
// take-down in two halves and only one of them needs a safe point (see the record in
// `core::interceptor`), and the shape that needs one is the door: `guard::coroutine_trip` takes a door
// down from inside the door's own frame, while that frame is mid-call on the game's `MoveNext` through
// the door's own trampoline, and the arm may call through that trampoline again after the trip. Every
// other take-down this fork makes is completed in the frame that asked for it.
//
// The record is filled from the method the class table declares, not from the request that asked for
// it and not from the site that arms the hook, because the doors here are armed from two different
// kinds of site: `IEnumerator::hook_move_next` (the six `Screen` doors, `GameSystem::
// InitializeGame_MoveNext`, `LiveViewController::GetChangeViewOrientation_MoveNext`,
// `UIManager::WaitBootSetup_MoveNext`) and a `new_hook!` on an address some resolver published
// (`CutStateProbe::PlayTrainingCutStateMachine_MoveNext`, resolved through
// `AnimationSpeed::resolve_method`, which runs `get_method_overloads` right here). Both walks pass
// through this file, so the signature is the only place the two have in common.
//
// Written while resolving - at `init`, and on the first `MoveNext` ask of each enumerator class, both
// behind `MOVE_NEXT_CACHE` - and read only when a hook is being taken down. Nothing on a path a detour
// runs per call reads or writes it (AGENTS section 6).
static COROUTINE_DOOR_TARGETS: Lazy<Mutex<FnvHashSet<usize>>> = Lazy::new(|| Mutex::default());

/// The signature a coroutine door stands on: the instance `MoveNext/0 -> bool()` a compiler generated
/// enumerator carries, which is the method Unity drives a live coroutine through and the one
/// `MoveNextFn` declares. Pure, so the rule that splits the two take-down shapes is testable the way
/// `select_overload` is; the half that reads a class table is not.
///
/// The static half of the name is refused because a static `MoveNext` is not the method a wrapper
/// with a `this` register calls (A3), and a `MoveNext` that takes parameters is not the step Unity
/// calls. Both are enumerated by name in `find_move_next_addr`, which refuses them for the same reason.
fn method_is_coroutine_door(name: &str, param_count: u32, ret: Option<Il2CppTypeEnum>, is_static: bool) -> bool {
    name == "MoveNext"
        && param_count == 0
        && ret == Some(Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN)
        && !is_static
}

/// Put the address of a method this file just read off a class table into the door record. An address
/// is only ever recorded from a `MethodInfo` the game declared, and 0 - the answer a lookup gives when
/// it found nothing - is never recorded.
pub(crate) fn record_coroutine_door_target(addr: usize) {
    if addr == 0 {
        return;
    }

    // C2: a lock an earlier panic poisoned still holds real entries, so the map is handed back rather
    // than turning a later take-down into a second panic.
    COROUTINE_DOOR_TARGETS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).insert(addr);
}

/// The recording half both class-table walks call: the four values the game declares about one method,
/// and that method's address. When they are the door's, the address goes in the record. Cold - a walk
/// over a class table happens while a hook is being resolved, never per call.
fn record_coroutine_door_if_the_signature_says_so(
    name: &str,
    param_count: u32,
    ret: Option<Il2CppTypeEnum>,
    is_static: bool,
    method_pointer: usize,
) {
    if method_is_coroutine_door(name, param_count, ret, is_static) {
        record_coroutine_door_target(method_pointer);
    }
}

fn record_door_from_method(method: *const MethodInfo, param_count: u32) {
    // The name is the game's, and a name this file cannot read is not a name to compare (C2) - the
    // same refusal `walk_method_overloads` applies.
    let name = match unsafe { CStr::from_ptr((*method).name) }.to_str() {
        Ok(name) => name,
        Err(_) => return,
    };

    if name != "MoveNext" {
        return;
    }

    let return_type = unsafe { (*method).return_type };
    let is_static = unsafe { (*method).flags } & METHOD_ATTRIBUTE_STATIC != 0;

    record_coroutine_door_if_the_signature_says_so(
        name,
        param_count,
        if return_type.is_null() { None } else { Some(unsafe { (*return_type).type_() }) },
        is_static,
        unsafe { (*method).methodPointer },
    );
}

/// Whether the method at `addr` is the method a coroutine door stands on, as the record says. Read on
/// the cold path only: `Interceptor::unhook` asks it once per take-down to decide whether that
/// take-down waits for the game tick or runs where it was asked for.
pub(crate) fn is_coroutine_door_target(addr: usize) -> bool {
    if addr == 0 {
        return false;
    }

    COROUTINE_DOOR_TARGETS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).contains(&addr)
}

// The signature `MoveNextFn` declares: `MoveNext/0 -> bool()` on an instance method of the
// enumerator. It is the method Unity drives a live coroutine through.
fn move_next_request() -> MethodRequest<'static> {
    MethodRequest {
        params: &[],
        ret: Some(Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN),
        allow_static: false,
        require_static: false,
        generic_slots: false,
    }
}

/// The `MoveNext` of an enumerator class that carries the signature the coroutine wrappers here are
/// written against, as the address a hook is installed on.
///
/// The lookup used to be `get_method_addr_cached(class, c"MoveNext", 0)`: a name plus argument count
/// answer, cached per class, handed straight to a `MoveNextFn` whatever that method really returns
/// or whether it is static. `hook_move_next` installs an abort - the detour answers the game's own
/// coroutine machinery `false` and the coroutine stops - on every coroutine of that class, so the
/// method it stands on is the decision that has to be right. A `MoveNext` that is not the instance
/// `() -> bool` the wrapper describes is refused here instead of hooked, and a class whose table
/// offers more than one is reported rather than settled by the order of that table (C15).
///
/// On this client that refusal is latent, and it is worth saying which half of C15 is not. The
/// dump prints 34 `MoveNext` lines over 34 enumerator classes and every one of them is the
/// instance `MoveNext/0 -> bool()` the wrapper describes, with no enumerator class carrying two,
/// so the refusal drops no coroutine hook today (the scan behind that count is in the C15
/// concept). What the class-wide install really means here is that the detour stands on the
/// compiler generated class, not on one coroutine, and the only thing that takes it down is a
/// barrier trip on that door.
///
/// Barrier item 2 is about how that take-down happens. `guard::coroutine_trip` calls
/// `Interceptor::unhook` from inside the door's own frame, and the backend half of a take-down -
/// `MH_DisableHook` plus `MH_RemoveHook` - would put this method's bytes back and let its trampoline go
/// under a thread still executing out of it, silently, and for a door armed once in `init` and never
/// re-armed - `CutStateProbe`'s `PlayTrainingCutStateMachine_MoveNext` is one - once was enough to lose
/// it for the session. `Interceptor::unhook` splits it: the door leaves the registry on the spot and
/// names itself in the log, and the backend half waits for `Interceptor::drain_deferred_unhooks`, which
/// `GameSystem::GameSystem_Update` runs on the game tick. That queue is scoped to doors, and the fact it
/// is scoped on lives here: the address resolved below is recorded in `COROUTINE_DOOR_TARGETS` while the
/// class table is read, which is why a door armed through `hook_move_next` and a door armed by a
/// `new_hook!` on `AnimationSpeed::resolve_method`'s answer - the shape the cut probe uses - are treated
/// the same way, and why a hook on anything else is taken down in the frame that asked for it instead of
/// waiting for a tick that is not a safe point for it.
/// What the class-wide install costs when the game starts a second coroutine of the class it was
/// installed on is still what the C16 note in `hook/umamusume/GameSystem.rs` says.
///
/// The answer is cached per class because the question is asked the same way every time a coroutine
/// is started, and `free_camera`'s enumerator iteration asks it on a per frame path. The cache
/// carries the signature check with it: an address is only cached once the method behind it has been
/// read, and a class that was refused is cached as 0 so the refusal says itself once.
fn find_move_next_addr(class: *mut Il2CppClass) -> Result<usize, Error> {
    let key = class as usize;
    let mut cache = MOVE_NEXT_CACHE.lock().unwrap_or_else(|e| e.into_inner());

    if let Some(cached) = cache.get(&key).copied() {
        return match cached {
            0 => Err(Error::MethodNotFound("MoveNext".to_owned())),
            addr => Ok(addr),
        };
    }

    let request = move_next_request();
    let candidates = get_method_overloads(class, "MoveNext", &request);
    let answer = select_overload(&candidates, &request);

    let addr = match answer {
        OverloadAnswer::Unique(index) => unsafe { (*candidates[index].method).methodPointer },
        OverloadAnswer::None => {
            if candidates.is_empty() {
                warn!("find_move_next: MoveNext = NULL on this enumerator class");
            }
            else {
                warn!(
                    "find_move_next: {} method(s) named MoveNext on this enumerator class are not the instance `() -> bool` the coroutine wrapper declares",
                    candidates.len()
                );
            }

            0
        },
        OverloadAnswer::Ambiguous(count) => {
            warn!(
                "find_move_next: {} methods on this enumerator class carry MoveNext/0 -> bool; the coroutine abort is not installed on an ambiguous one",
                count
            );

            0
        },
    };

    // C9: this is the same stale-record trade `METHOD_CACHE` already makes. A cached `MethodInfo`
    // can go stale if metadata is rebuilt under a live hook, and the hook itself stays installed on
    // the address it was installed on either way; re-walking the table per frame would not undo that.
    cache.insert(key, addr);

    match addr {
        0 => Err(Error::MethodNotFound("MoveNext".to_owned())),
        addr => Ok(addr),
    }
}

impl<T> IEnumerator<T> {
    pub fn iter(&self) -> Option<IEnumeratorIterator<T>> {
        if self.this.is_null() {
            return None;
        }

        let class = unsafe { (*self.this).klass() };
        // Get addr manually to avoid nullptr warning. `MoveNext` is resolved by the signature the
        // iterator wrapper declares (C15); `get_Current` is still the name plus argument count
        // lookup (C7), and the two spellings a state machine carries - the `IEnumerator` one and the
        // `IEnumerator<T>` one - answer the same reference this wrapper holds.
        let get_current_method = get_method_cached(class, c"get_Current", 0);
        let get_current_addr = get_current_method.map(|m| unsafe { (*m).methodPointer }).unwrap_or(0);
        let move_next_addr = match find_move_next_addr(class) {
            Ok(addr) => addr,
            Err(_) => return None,
        };

        if move_next_addr == 0 {
            return None;
        }

        Some(IEnumeratorIterator {
            this: self.this,
            get_Current: unsafe { std::mem::transmute(get_current_addr) },
            MoveNext: unsafe { std::mem::transmute(move_next_addr) }
        })
    }

    pub fn hook_move_next(&self, hook_fn: MoveNextFn) -> Result<usize, Error> {
        // C9: `self.this` here is the coroutine object a game method just returned, and a game
        // method that returns no coroutine answers null. There is no class to take a `MoveNext` off
        // of that, and `(*null).klass` is a read at address 0.
        if self.this.is_null() {
            return Err(Error::MethodNotFound("MoveNext".to_owned()));
        }

        let class = unsafe { (*self.this).klass() };
        let move_next_addr = find_move_next_addr(class)?;

        // The door is armed on the enumerator class, not on this coroutine (C15), so a later coroutine
        // of the same class asks for the same wrapper again - and if a barrier trip already took that
        // door down, `hook` releases the target the queue is still holding before it re-arms it
        // (C2, barrier item 2).
        Hachimi::instance().interceptor.hook(move_next_addr, hook_fn as usize)
    }
}

impl<T> From<*mut Il2CppObject> for IEnumerator<T> {
    fn from(value: *mut Il2CppObject) -> Self {
        IEnumerator {
            this: value,
            _phantom: PhantomData
        }
    }
}

#[allow(non_snake_case)]
pub struct IEnumeratorIterator<T> {
    this: *mut Il2CppObject,
    get_Current: Option<extern "C" fn(*mut Il2CppObject) -> T>,
    MoveNext: MoveNextFn
}

impl<T> Iterator for IEnumeratorIterator<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        // TODO: properly handle enumerators that returns nothing
        let Some(get_current) = self.get_Current else {
            return None;
        };

        if (self.MoveNext)(self.this) {
            Some(get_current(self.this))
        }
        else {
            None
        }
    }
}

#[allow(non_snake_case)]
pub struct IList<T = *mut Il2CppObject> {
    pub this: *mut Il2CppObject,
    get_Item: extern "C" fn(*mut Il2CppObject, i32) -> T,
    set_Item: extern "C" fn(*mut Il2CppObject, i32, T),
    get_Count: extern "C" fn(*mut Il2CppObject) -> i32
}

impl<T> IList<T> {
    pub fn new(this: *mut Il2CppObject) -> Option<IList<T>> {
        if this.is_null() {
            return None;
        }

        let class = unsafe { (*this).klass() };
        let get_item_addr = get_method_addr_cached(class, c"get_Item", 1);
        let set_item_addr = get_method_addr_cached(class, c"set_Item", 2);
        let get_count_addr = get_method_addr_cached(class, c"get_Count", 0);

        if get_item_addr == 0 || set_item_addr == 0 || get_count_addr == 0 {
            return None;
        }       

        Some(IList {
            this,
            get_Item: unsafe { std::mem::transmute(get_item_addr) },
            set_Item: unsafe { std::mem::transmute(set_item_addr) },
            get_Count: unsafe { std::mem::transmute(get_count_addr) }
        })
    }

    /// Returns `None` if `i` is out of range.
    pub fn get(&self, i: i32) -> Option<T> {
        if i >= 0 && i < self.count() {
            Some((self.get_Item)(self.this, i))
        }
        else {
            None
        }
    }

    /// Returns `false` if `i` is out of range.
    pub fn set(&self, i: i32, value: T) -> bool {
        if i >= 0 && i < self.count() {
            (self.set_Item)(self.this, i, value);
            true
        }
        else {
            false
        }
    }

    pub fn count(&self) -> i32 {
        (self.get_Count)(self.this)
    }

    pub fn iter<'a>(&'a self) -> IListIter<'a, T> {
        IListIter { list: self, i: -1 }
    }
}

impl<'a, T> IntoIterator for &'a IList<T> {
    type Item = T;
    type IntoIter = IListIter<'a, T>;
    
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<T> Into<Vec<T>> for IList<T> {
    fn into(self) -> Vec<T> {
        self.iter().collect()
    }
}

pub struct IListIter<'a, T> {
    list: &'a IList<T>,
    i: i32
}

impl<'a, T> Iterator for IListIter<'a, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        self.i += 1;
        self.list.get(self.i)
    }
}

// IDictionary wrapper
#[allow(non_snake_case)]
pub struct IDictionary<K, V> {
    pub this: *mut Il2CppObject,
    get_Item: extern "C" fn(*mut Il2CppObject, K) -> V,
    set_Item: extern "C" fn(*mut Il2CppObject, K, V),
    Contains: extern "C" fn(*mut Il2CppObject, K) -> bool
}

impl<K, V> IDictionary<K, V> {
    pub fn new(this: *mut Il2CppObject) -> Option<IDictionary<K, V>> {
        if this.is_null() {
            return None;
        }

        let class = unsafe { (*this).klass() };
        let get_item_addr = get_method_addr_cached(class, c"get_Item", 1);
        let set_item_addr = get_method_addr_cached(class, c"set_Item", 2);
        let contains_addr = get_method_addr_cached(class, c"Contains", 1);

        if get_item_addr == 0 || set_item_addr == 0 || contains_addr == 0 {
            return None;
        }

        Some(IDictionary {
            this,
            get_Item: unsafe { std::mem::transmute(get_item_addr) },
            set_Item: unsafe { std::mem::transmute(set_item_addr) },
            Contains: unsafe { std::mem::transmute(contains_addr) }
        })
    }

    pub fn get(&self, key: K) -> V {
        (self.get_Item)(self.this, key)
    }

    pub fn set(&self, key: K, value: V) {
        (self.set_Item)(self.this, key, value);
    }

    pub fn contains(&self, key: K) -> bool {
        (self.Contains)(self.this, key)
    }
}

// Il2CppThread wrapper
#[repr(transparent)]
#[derive(Clone)]
pub struct Thread(*mut Il2CppThread);

impl Thread {
    pub fn from_raw(ptr: *mut Il2CppThread) -> Self {
        Self(ptr)
    }

    fn sync_ctx(&self) -> *mut Il2CppObject {
        // C9: the thread pointer is the game's, from the attached-thread list or from a caller.
        let thread = match unsafe { self.0.as_ref() } { Some(thread) => thread, None => return null_mut() };

        let class = thread.obj.klass();
        let get_exec_ctx_addr = get_method_addr_cached(class, c"GetMutableExecutionContext", 0);
        if get_exec_ctx_addr == 0 {
            return null_mut();
        }

        let get_exec_ctx: extern "C" fn(*mut Il2CppObject) -> *mut Il2CppObject = unsafe {
            std::mem::transmute(get_exec_ctx_addr)
        };
        let exec_ctx = get_exec_ctx(self.0 as *mut Il2CppObject);

        // C9: a thread that never built its context answers null, and that is the honest answer
        // here - `schedule` already says the callback was not scheduled.
        if exec_ctx.is_null() {
            return null_mut();
        }

        let exec_ctx_class = unsafe { (*exec_ctx).klass() };

        let sync_ctx_field = il2cpp_class_get_field_from_name(exec_ctx_class, c"_syncContext".as_ptr());
        if sync_ctx_field.is_null() {
            return null_mut();
        }

        get_field_object_value(exec_ctx, sync_ctx_field)
    }

    pub fn schedule(&self, callback: fn()) {
        let sync_ctx = self.sync_ctx();
        if sync_ctx.is_null() {
            error!("synchronization context is null, callback not scheduled");
            return;
        }
        let sync_ctx_class = unsafe { (*sync_ctx).klass() };

        let sync_ctx_post: extern "C" fn(*mut Il2CppObject, *mut Il2CppDelegate, *mut Il2CppObject) = unsafe {
            std::mem::transmute(get_method_addr_cached(sync_ctx_class, c"Post", 2))
        };

        let mscorlib = get_assembly_image(c"mscorlib.dll").expect("mscorlib");
        let delegate_class = get_class(mscorlib, c"System.Threading", c"SendOrPostCallback").expect("SendOrPostCallback");
        let delegate = create_delegate(delegate_class, 1, callback).unwrap();

        sync_ctx_post(sync_ctx, delegate, null_mut());
    }

    pub fn attached_threads() -> &'static [Thread] {
        let mut size = 0;
        let list_ptr = il2cpp_thread_get_all_attached_threads(&mut size);

        // C9: the array is the game's, and null with nothing attached is an answer; a slice over it
        // would be built from address 0.
        if list_ptr.is_null() {
            return &[];
        }

        unsafe { std::slice::from_raw_parts(list_ptr as *const Thread, size) }
    }

    pub fn main_thread() -> Thread {
        Self::attached_threads().get(0).expect("main thread must be present").clone()
    }

    pub fn as_raw(&self) -> *mut Il2CppThread {
        self.0
    }
}

// Delegate creation
pub fn create_delegate(delegate_class: *mut Il2CppClass, args_count: i32, method_ptr: fn()) -> Option<*mut Il2CppDelegate> {
    let delegate_invoke = get_method_cached(delegate_class, c"Invoke", args_count).ok()?;
    
    let delegate_ctor_addr = get_method_addr_cached(delegate_class, c".ctor", 2);
    if delegate_ctor_addr == 0 {
        return None;
    }
    let delegate_ctor: extern "C" fn(*mut Il2CppObject, *mut Il2CppObject, *const MethodInfo) = unsafe {
        std::mem::transmute(delegate_ctor_addr)
    };

    let delegate_obj = il2cpp_object_new(delegate_class);

    // C9: allocation is the game's to refuse. A delegate that was not created has no slot to write
    // the method pointer into, so the creation reports failure instead of writing at address 0.
    if delegate_obj.is_null() {
        return None;
    }

    delegate_ctor(delegate_obj, delegate_obj, delegate_invoke);
    let delegate = delegate_obj as *mut Il2CppDelegate;
    unsafe {
        (*delegate).method_ptr = method_ptr as _;
        (*delegate).invoke_impl = method_ptr as _;
    }

    Some(delegate)
}

// Singleton-like class wrapper
pub struct SingletonLike {
    get_instance_method: *const MethodInfo,
}

impl SingletonLike {
    pub fn new(class: *mut Il2CppClass) -> Option<SingletonLike> {
        let method = il2cpp_class_get_method_from_name(class, c"get_Instance".as_ptr(), 0);
        if method.is_null() {
            warn!("SingletonLike: get_Instance method not found");
            return None;
        }

        Some(SingletonLike {
            get_instance_method: method
        })
    }

    /// Rebuilds a `SingletonLike` from a `MethodInfo` pointer kept as a `usize`. A path that asks for a
    /// singleton on every game tick cannot hold the pointer in a static (a raw pointer is not `Sync`),
    /// but it can hold the number, and that lets the method be resolved once instead of by name per call.
    pub const fn from_method_ptr(method: usize) -> Self {
        Self {
            get_instance_method: method as *const MethodInfo
        }
    }

    pub fn instance(&self) -> *mut Il2CppObject {
        let mut exc: *mut Il2CppException = null_mut();
        let obj = il2cpp_runtime_invoke(
            self.get_instance_method,
            null_mut(),
            std::ptr::null_mut(),
            &mut exc
        );
        if !exc.is_null() {
            warn!("SingletonLike: get_Instance threw an exception");
        }
        obj
    }
}

// GCHandle wrapper
#[repr(transparent)]
pub struct GCHandle(u32);

impl GCHandle {
    pub fn new(obj: *mut Il2CppObject, pinned: bool) -> GCHandle {
        GCHandle(il2cpp_gchandle_new(obj, pinned))
    }

    pub fn new_weak_ref(obj: *mut Il2CppObject, track_resurrection: bool) -> GCHandle {
        GCHandle(il2cpp_gchandle_new_weakref(obj, track_resurrection))
    }

    pub fn target(&self) -> *mut Il2CppObject {
        il2cpp_gchandle_get_target(self.0)
    }
}

impl Drop for GCHandle {
    fn drop(&mut self) {
        il2cpp_gchandle_free(self.0);
    }
}

// Il2CppArray wrapper
#[repr(transparent)]
pub struct Array<T = *mut Il2CppObject> {
    pub this: *mut Il2CppArray,
    _phantom: PhantomData<T>
}

impl<T> Array<T> {
    pub fn new(element_type: *mut Il2CppClass, length: il2cpp_array_size_t) -> Array<T> {
        Array {
            this: il2cpp_array_new(element_type, length),
            _phantom: PhantomData,
        }
    }

    pub unsafe fn data_ptr(&self) -> *mut T {
        // C9: `Array::from` accepts whatever the game handed back, including null, so the refusal
        // is here: no array has no element storage.
        if self.this.is_null() {
            return null_mut();
        }

        self.this.add(1) as _
    }

    pub unsafe fn as_slice(&self) -> &mut [T] {
        // C9: a null array is an empty one. A reference field nobody assigned, or a getter that
        // returned nothing, is walked as nothing rather than as `max_length` bytes at address 0.
        if self.this.is_null() {
            return &mut [];
        }

        std::slice::from_raw_parts_mut(self.data_ptr(), (*self.this).max_length)
    }

    pub fn len(&self) -> usize {
        // C9: same rule as `as_slice` - nothing to walk is the answer a caller of `len` already
        // handles.
        match unsafe { self.this.as_ref() } {
            Some(array) => array.max_length as usize,
            None => 0,
        }
    }
}

impl<T> Into<*mut Il2CppArray> for Array<T> {
    fn into(self) -> *mut Il2CppArray {
        self.this
    }
}

impl<T> From<*mut Il2CppArray> for Array<T> {
    fn from(value: *mut Il2CppArray) -> Self {
        Self {
            this: value,
            _phantom: PhantomData
        }
    }
}

pub struct FieldsIter {
    class: *mut Il2CppClass,
    iter: *mut c_void
}

impl FieldsIter {
    pub fn new(class: *mut Il2CppClass) -> Self {
        Self {
            class,
            iter: null_mut()
        }
    }
}

impl Iterator for FieldsIter {
    type Item = *mut FieldInfo;

    fn next(&mut self) -> Option<Self::Item> {
        let field = il2cpp_class_get_fields(self.class, &mut self.iter);
        if field.is_null() {
            return None;
        }
        Some(field)
    }
}

#[repr(C)]
pub struct Il2CppDictionary {
    pub obj: Il2CppObject,
    pub buckets: *mut Il2CppArray,
    pub entries: *mut Il2CppArray,
    pub count: i32,
    /* STUB */
}

#[repr(C)]
pub struct Il2CppDictionaryEntry<K, V> {
    pub hash_code: i32,
    pub next: i32,
    pub key: K,
    pub value: V
}

// Generic Dictionary wrapper
#[repr(transparent)]
pub struct Dictionary<K, V> {
    pub this: *mut Il2CppDictionary,
    _k: PhantomData<K>,
    _v: PhantomData<V>
}

impl<K, V> Into<*mut Il2CppDictionary> for Dictionary<K, V> {
    fn into(self) -> *mut Il2CppDictionary {
        self.this
    }
}

impl<K, V> From<*mut Il2CppDictionary> for Dictionary<K, V> {
    fn from(value: *mut Il2CppDictionary) -> Self {
        Self {
            this: value,
            _k: PhantomData,
            _v: PhantomData
        }
    }
}

impl<K, V> Dictionary<K, V> {
    pub fn buckets(&self) -> Array<i32> {
        // C9: the backing arrays belong to the game. `find_entry` already reads a dictionary that
        // never got its buckets as an empty one, so these say the same thing instead of loading
        // `(*null).buckets`.
        let Some(this) = (unsafe { self.this.as_ref() }) else {
            return Array::from(null_mut());
        };

        this.buckets.into()
    }

    pub fn entries(&self) -> Array<Il2CppDictionaryEntry<K, V>> {
        let Some(this) = (unsafe { self.this.as_ref() }) else {
            return Array::from(null_mut());
        };

        this.entries.into()
    }

    pub fn count(&self) -> i32 {
        let Some(this) = (unsafe { self.this.as_ref() }) else {
            return 0;
        };

        this.count
    }
}

impl<K: PartialEq, V> Dictionary<K, V> {
    pub fn find_entry(&self, key: &K) -> Option<&'static mut Il2CppDictionaryEntry<K, V>> {
        if self.this.is_null() || unsafe { (*self.this).entries.is_null() } {
            return None;
        }
        for entry in unsafe { self.entries().as_slice().iter_mut() } {
            if entry.key == *key {
                // freaky lifetime erasure
                return unsafe { std::ptr::from_mut(entry).as_mut() };
            }
        }

        None
    }
}

impl<K: PartialEq + 'static, V> Dictionary<K, V> {
    pub fn get(&self, key: &K) -> Option<&'static mut V> {
        self.find_entry(&key).map(|e| &mut e.value)
    }
}

// Generic IL2CPP enum and type utilities

pub fn get_runtime_type(asm: &CStr, ns: &CStr, name: &CStr) -> *mut Il2CppObject {
    let k = match get_class(match get_assembly_image(asm) {
        Ok(img) => img,
        Err(_) => return null_mut()
    }, ns, name) {
        Ok(c) => c,
        Err(_) => return null_mut()
    };
    if k.is_null() { return null_mut(); }
    let t = il2cpp_class_get_type(k);
    if t.is_null() { return null_mut(); }
    il2cpp_type_get_object(t) as *mut Il2CppObject
}

pub fn parse_enum(enum_type: *mut Il2CppObject, value: &str) -> Option<*mut Il2CppObject> {
    if enum_type.is_null() || value.is_empty() { return None; }
    let enum_class = match get_class(
        match get_assembly_image(c"mscorlib.dll") { Ok(img) => img, Err(_) => return None },
        c"System", c"Enum"
    ) {
        Ok(c) => c,
        Err(_) => return None
    };
    let val_str = crate::il2cpp::ext::StringExt::to_il2cpp_string(value);
    let parse_method = get_method_cached(enum_class, c"Parse", 2).ok()?;
    let mut params: [*mut c_void; 2] = [enum_type as *mut c_void, val_str as *mut c_void];
    let mut exc = null_mut();
    let result = il2cpp_runtime_invoke(parse_method, null_mut(), params.as_mut_ptr(), &mut exc);
    if !exc.is_null() || result.is_null() { None } else { Some(result as *mut Il2CppObject) }
}

pub fn get_enum_int(e: *mut Il2CppObject) -> i32 {
    if e.is_null() { return 0; }
    let enum_class = match get_class(
        match get_assembly_image(c"mscorlib.dll") { Ok(img) => img, Err(_) => return 0 },
        c"System", c"Enum"
    ) {
        Ok(c) => c,
        Err(_) => return 0
    };
    let to_uint64_method = match get_method_cached(enum_class, c"ToUInt64", 1) {
        Ok(m) => m,
        Err(_) => return 0
    };
    let mut params: [*mut c_void; 1] = [e as *mut c_void];
    let mut exc = null_mut();
    let result = il2cpp_runtime_invoke(to_uint64_method, null_mut(), params.as_mut_ptr(), &mut exc);
    if !exc.is_null() || result.is_null() { return 0; }
    unsafe { *(il2cpp_object_unbox(result) as *const u64) as i32 }
}

pub fn get_type_object_for_class(klass: *mut Il2CppClass) -> *mut Il2CppObject {    if klass.is_null() { return null_mut(); }
    let t = il2cpp_class_get_type(klass);
    if t.is_null() { return null_mut(); }
    il2cpp_type_get_object(t) as *mut Il2CppObject
}

pub fn invoke_object_method(
    obj: *mut Il2CppObject,
    method_name: &CStr,
    param_count: i32,
    params: &mut [*mut c_void]
) -> Option<*mut Il2CppObject> {
    // C9: the object is the game's. Asked to call a method on nothing there is no class to find
    // the method on, and `(*null).klass` is a read at address 0.
    if obj.is_null() {
        return None;
    }

    let klass = unsafe { (*obj).klass() };
    let method = get_method_cached(klass, method_name, param_count).ok()?;
    let mut exc = null_mut();
    let result = il2cpp_runtime_invoke(
        method,
        obj as *mut c_void,
        params.as_mut_ptr(),
        &mut exc
    );
    if !exc.is_null() { None } else { Some(result as *mut Il2CppObject) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::null;

    #[test]
    fn a_declared_pointer_slot_answers_a_generic_instantiation_only_when_asked() {
        // The exact walk a scaling hook uses: a generic is not a plain class reference.
        assert!(param_type_accepts(Il2CppTypeEnum_IL2CPP_TYPE_CLASS, Il2CppTypeEnum_IL2CPP_TYPE_CLASS, false));
        assert!(!param_type_accepts(Il2CppTypeEnum_IL2CPP_TYPE_CLASS, Il2CppTypeEnum_IL2CPP_TYPE_GENERICINST, false));

        // The wider walk, for a wrapper that declares a pointer where the dump spells a generic.
        assert!(param_type_accepts(Il2CppTypeEnum_IL2CPP_TYPE_CLASS, Il2CppTypeEnum_IL2CPP_TYPE_GENERICINST, true));
        assert!(param_type_accepts(Il2CppTypeEnum_IL2CPP_TYPE_OBJECT, Il2CppTypeEnum_IL2CPP_TYPE_GENERICINST, true));

        // A slot that declared a value never has a reference handed to it, and a widening never
        // turns a value request into a pointer one.
        assert!(!param_type_accepts(Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE, Il2CppTypeEnum_IL2CPP_TYPE_GENERICINST, true));
        assert!(!param_type_accepts(Il2CppTypeEnum_IL2CPP_TYPE_R4, Il2CppTypeEnum_IL2CPP_TYPE_CLASS, true));
        assert!(!param_type_accepts(Il2CppTypeEnum_IL2CPP_TYPE_CLASS, Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE, true));
    }

    // C9: the field accessors here are the chokepoint - every `def_field_value_accessors!` and
    // `def_field_object_accessors!` getter in the crate routes through them. Two different nulls
    // arrive: a handle this client never resolved (`get_field_from_name` logs it and answers null),
    // and an object the game answered null for, which is a real answer (an unassigned reference
    // field, a list slot nobody filled, a singleton that is not live). Neither may reach the game's
    // own getter, because `il2cpp_field_get_value` would read at (0 + offset).
    //
    // The dangling addresses below are never touched: what the test asserts is that each helper
    // refuses before it asks the game for anything. Every `il2cpp_*` entry point in this crate is a
    // `lazy_fnptr!` resolved at first use, so a guard that returns first also proves nothing was
    // reached - an unguarded call in a test process would die on the dlsym, not on the pointer.
    #[test]
    fn a_field_with_no_object_or_a_name_with_no_field_answers_without_reaching_the_game() {
        let no_object = null_mut::<Il2CppObject>();
        let no_field = null_mut::<FieldInfo>();
        let a_field_handle = 0x10usize as *mut FieldInfo; // stand-in for a resolved handle
        let an_object = 0x1000usize as *mut Il2CppObject; // stand-in for a live object

        assert_eq!(get_field_value::<i32>(no_object, no_field), 0);
        assert_eq!(get_field_value::<i32>(an_object, no_field), 0);
        assert_eq!(get_field_value::<f32>(no_object, a_field_handle), 0.0);
        assert!(get_field_object_value::<Il2CppObject>(no_object, a_field_handle).is_null());

        // A slot address is only useful to a caller that checks it, so the refusal is a null address
        // rather than an address built from a null handle or a null object.
        assert!(get_field_ptr::<f32>(no_object, a_field_handle).is_null());
        assert!(get_field_ptr::<f32>(an_object, no_field).is_null());

        assert_eq!(get_static_field_value::<i32>(no_field), 0);
        assert_eq!(get_static_field_value::<f32>(no_field), 0.0);
        assert!(get_static_field_object_value::<Il2CppObject>(no_field).is_null());

        // Writes are refused the same way: no call reaches the game, and nothing is stored.
        set_field_value(no_object, a_field_handle, &1i32);
        set_field_value(an_object, no_field, &1i32);
        set_static_field_value(no_field, 1i32);
        assert_eq!(get_static_field_value::<i32>(no_field), 0);

        // A boxed value nobody filled is null, and its unbox answers null.
        assert_eq!(unsafe { unbox::<i32>(no_object) }, 0);
    }

    #[test]
    fn a_container_the_game_did_not_hand_over_is_an_empty_one() {
        // `Array::from` and `Dictionary::from` accept whatever a game getter returned, including
        // null, so the refusal has to be in the readers.
        let no_array: Array<*mut Il2CppObject> = Array::from(null_mut());
        assert_eq!(no_array.len(), 0);
        assert!(unsafe { no_array.as_slice() }.is_empty());
        assert!(unsafe { no_array.data_ptr() }.is_null());

        let no_dict: Dictionary<i32, *mut Il2CppObject> = Dictionary::from(null_mut());
        assert_eq!(no_dict.count(), 0);
        assert!(no_dict.find_entry(&1).is_none());
        assert!(no_dict.get(&1).is_none());
        assert_eq!(no_dict.buckets().len(), 0);
        assert!(no_dict.entries().this.is_null());

        // A null enumerator has no class to take a `MoveNext` off.
        let no_enumerator: IEnumerator = IEnumerator::from(null_mut());
        assert!(no_enumerator.iter().is_none());
        assert!(no_enumerator.hook_move_next(never_moves).is_err());
    }

    extern "C" fn never_moves(_enumerator: *mut Il2CppObject) -> bool {
        false
    }

    // C15. The candidate list and the decision run on it, on the shapes this client's own dump
    // prints. `method` is never read by the decision, so the candidates below stand for methods
    // with the return type and the static bit the dump spells for them.
    fn candidate(ret: Option<Il2CppTypeEnum>, is_static: bool) -> OverloadCandidate {
        OverloadCandidate { method: null(), ret, is_static }
    }

    fn request(ret: Option<Il2CppTypeEnum>, allow_static: bool, require_static: bool) -> MethodRequest<'static> {
        MethodRequest { params: &[], ret, allow_static, require_static, generic_slots: false }
    }

    // `Gallop.ModelController::GetBodyShader/2` (introspect.log:1188-1189): a `static` overload and
    // an instance one, under one name, one argument count and the same two parameter enums. On that
    // pair the old walk - name, argument count, parameter list, first match wins - answers the
    // static one because the class table lists it first, and a caller that checks the static bit
    // afterwards then refuses what it was handed. No site in this tree asks for `GetBodyShader`, so
    // that is what the pair would do to a wrapper written against it, not something this client has
    // been seen to do; the C15 concept counts the pairs of this shape the dump has and names the
    // ones a hook site reaches.
    #[test]
    fn a_static_pair_under_one_name_answers_the_half_the_wrapper_asked_for() {
        let get_body_shader = [
            candidate(Some(Il2CppTypeEnum_IL2CPP_TYPE_CLASS), true),   // 1188: static
            candidate(Some(Il2CppTypeEnum_IL2CPP_TYPE_CLASS), false),  // 1189: instance
        ];

        // A wrapper that reserves a register for `this`: the instance overload carries it.
        let instance_wrapper = request(Some(Il2CppTypeEnum_IL2CPP_TYPE_CLASS), false, false);
        assert_eq!(select_overload(&get_body_shader, &instance_wrapper), OverloadAnswer::Unique(1),
            "a wrapper written for the instance half was refused by the static half listed next to it");

        // A wrapper that declares no `this`: the same pair answers the other way round.
        let static_wrapper = request(Some(Il2CppTypeEnum_IL2CPP_TYPE_CLASS), true, true);
        assert_eq!(select_overload(&get_body_shader, &static_wrapper), OverloadAnswer::Unique(0),
            "a wrapper written for the static half was refused by the instance half listed next to it");

        // And the refusal says which of the two it looked at, which is what the install line prints.
        assert_eq!(
            overload_rejects_request(&get_body_shader[0], &instance_wrapper),
            Some(OverloadRejection::Staticness { candidate_is_static: true })
        );
        assert_eq!(
            overload_rejects_request(&get_body_shader[1], &static_wrapper),
            Some(OverloadRejection::Staticness { candidate_is_static: false })
        );

        // The before and after, on the pair the dump prints. Name plus arity plus parameter list
        // alone answers candidate 0 for `GetBodyShader/2` and stops there, and a caller that wanted
        // candidate 1 has to refuse what it was handed; the static bit travelling inside the
        // request answers which of the two a wrapper is, so the pair cannot decide the install by
        // the order the class table happens to list it in.
        println!(
            "GetBodyShader/2: name+arity+params alone binds candidate 0 (static {}); the instance wrapper's request binds candidate {} (static {})",
            get_body_shader[0].is_static,
            match select_overload(&get_body_shader, &instance_wrapper) {
                OverloadAnswer::Unique(index) => index,
                _ => usize::MAX,
            },
            get_body_shader[1].is_static,
        );
    }

    // A method that answers something else is not the method that was asked for, whatever its name
    // and argument count say. The door this file's callers use most is `PlayIn/4 -> void(float, int,
    // bool, class)` at `introspect.log:23748`, and the coroutine that stands next to it in the same
    // class is a different name, `CoroutinePlayIn/2` at 23778 - so this client offers no pair of the
    // shape the two assertions below pin: the scan in the C15 concept finds no class whose two members
    // under one name take the same parameters and hand back different things. What the return type
    // inside the request buys is that such a pair cannot decide an install either.
    #[test]
    fn a_request_carries_the_return_type_it_was_asked_with() {
        let coroutine_sibling = candidate(Some(Il2CppTypeEnum_IL2CPP_TYPE_GENERICINST), false);
        let the_door = candidate(Some(Il2CppTypeEnum_IL2CPP_TYPE_VOID), false);

        let void_wrapper = request(Some(Il2CppTypeEnum_IL2CPP_TYPE_VOID), false, false);

        assert_eq!(select_overload(&[coroutine_sibling, the_door], &void_wrapper), OverloadAnswer::Unique(1),
            "the coroutine sibling listed first took the place the void wrapper asked for");
        assert_eq!(select_overload(&[the_door, coroutine_sibling], &void_wrapper), OverloadAnswer::Unique(0),
            "the same pair answered the other way depending on table order");

        // A getter asked for a float is not answered by a void method under that name either.
        let float_wrapper = request(Some(Il2CppTypeEnum_IL2CPP_TYPE_R4), true, false);
        assert_eq!(select_overload(&[the_door], &float_wrapper), OverloadAnswer::None);

        // C9: a return record the metadata did not fill is not a signature to bind.
        assert_eq!(
            select_overload(&[candidate(None, false)], &void_wrapper),
            OverloadAnswer::None,
            "a method whose return type could not be read was bound"
        );
        assert_eq!(
            overload_rejects_request(&candidate(None, false), &void_wrapper),
            Some(OverloadRejection::Return { actual: None, expected: Il2CppTypeEnum_IL2CPP_TYPE_VOID })
        );
    }

    // A2 is still open, and this is the honest shape of it: two reference overloads that differ only
    // in the class behind a slot are one request to a matcher that compares enums. `SingleModeResult
    // ContentBase::FadeInContentFromRight/3` takes `UnityEngine.CanvasGroup` (introspect.log:24167)
    // and `UnityEngine.UI.MaskableGraphic` (24168). Both are kept, so the caller says the collision
    // out loud instead of letting the table's order pick one in silence.
    #[test]
    fn two_overloads_that_carry_the_same_signature_are_reported_as_two() {
        let fades = [candidate(Some(Il2CppTypeEnum_IL2CPP_TYPE_VOID), false), candidate(Some(Il2CppTypeEnum_IL2CPP_TYPE_VOID), false)];
        let fade_wrapper = request(Some(Il2CppTypeEnum_IL2CPP_TYPE_VOID), false, false);

        assert_eq!(select_overload(&fades, &fade_wrapper), OverloadAnswer::Ambiguous(2));
    }

    // The coroutine half of C15. `hook_move_next` installs a detour on the `MoveNext` of the
    // enumerator class the game handed back, and that detour stands on the class: every coroutine of
    // the class runs it, and the freeform window wrappers answer the game's coroutine machinery
    // `false` to stop one. The method it stands on has to be the `MoveNext/0 -> bool()` instance
    // method its wrapper describes, which is the decision these three answers are about. Today the
    // dump offers no enumerator class that would make them refuse: 34 `MoveNext` lines over 34
    // classes, every one of them the instance `MoveNext/0 -> bool()`, none carried twice, so this
    // test pins the rule and not an event - the C15 concept says which of the coroutine sites this
    // client can even reach.
    #[test]
    fn the_coroutine_abort_is_installed_only_on_a_move_next_that_is_the_wrapper_it_declares() {
        let request = move_next_request();

        let not_a_bool = candidate(Some(Il2CppTypeEnum_IL2CPP_TYPE_I4), false);
        let static_bool = candidate(Some(Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN), true);
        let move_next = candidate(Some(Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN), false);

        assert_eq!(select_overload(&[not_a_bool, static_bool], &request), OverloadAnswer::None,
            "a coroutine was stopped by a hook on a method that is not MoveNext/0 -> bool()");
        assert_eq!(select_overload(&[not_a_bool, static_bool, move_next], &request), OverloadAnswer::Unique(2),
            "the method next to it refused the real MoveNext");
        assert_eq!(select_overload(&[move_next, move_next], &request), OverloadAnswer::Ambiguous(2),
            "two candidates that both carry the signature are reported, not picked by table order");
    }

    // Barrier item 2 (C2) hands this file a question the interceptor cannot answer for itself: is the
    // method a hook stands on the method a coroutine door stands on? Only a take-down of that method is
    // deferred to the game tick, and the only place in this crate that reads a class table is here, so the
    // record is filled from the signature the game declares - not from the spelling of the request that
    // asked for the method, and not from which hook site armed it. That is what makes the two ways a door
    // is armed here agree: `IEnumerator::hook_move_next` (the `Screen` doors, `GameSystem::
    // InitializeGame_MoveNext`, `LiveViewController`, `UIManager`) resolves through `find_move_next_addr`,
    // and `CutStateProbe::PlayTrainingCutStateMachine_MoveNext` resolves through
    // `AnimationSpeed::resolve_method`; both walks end in `walk_method_overloads`, and both pass through
    // the recording below.
    //
    // What a test here cannot do is read a class table - no il2cpp in a test process (AGENTS section 4) -
    // so these drive the rule and the recording function the shipped walks call. The line a game run
    // proves the walk filled the record is the take-down line in `core::interceptor`.
    #[test]
    fn the_door_rule_reads_the_signature_a_coroutine_wrapper_declares_and_nothing_else() {
        let bool_ = Some(Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN);

        // The shape this client's dump prints for every one of its 34 enumerator classes.
        assert!(method_is_coroutine_door("MoveNext", 0, bool_, false));

        // A static `MoveNext` is not a method a wrapper that reserves a register for `this` calls (A3),
        // and one that takes parameters is not the step Unity drives a live coroutine through.
        // `find_move_next_addr` refuses both for the same reason, and a hook on neither is a take-down
        // that may claim the game tick.
        assert!(!method_is_coroutine_door("MoveNext", 0, bool_, true));
        assert!(!method_is_coroutine_door("MoveNext", 1, bool_, false));

        // Not the answer the wrapper reads: `MoveNextFn` returns the bool the game's coroutine machinery
        // asks for. A `MoveNext` answering something else is the C15 refusal, and an unfilled return
        // record is the C9 one.
        assert!(!method_is_coroutine_door("MoveNext", 0, Some(Il2CppTypeEnum_IL2CPP_TYPE_VOID), false));
        assert!(!method_is_coroutine_door("MoveNext", 0, Some(Il2CppTypeEnum_IL2CPP_TYPE_I4), false));
        assert!(!method_is_coroutine_door("MoveNext", 0, None, false));

        // The neighbours an enumerator carries next to the door, and the explicit-interface spelling a
        // name plus argument list lookup could otherwise hand to a hook.
        assert!(!method_is_coroutine_door("get_Current", 0, bool_, false));
        assert!(!method_is_coroutine_door("set_MoveNext", 0, bool_, false));
        assert!(!method_is_coroutine_door("System.Collections.IEnumerator.MoveNext", 0, bool_, false));
    }

    #[test]
    fn only_a_method_the_game_declares_as_the_door_is_recorded_as_one() {
        // Three stand-in `methodPointer`s, as a class table would publish them, carrying the three
        // shapes above. The recording function is the one `walk_method_overloads` calls per candidate.
        let door = 0x4a00usize;
        let static_sibling = 0x4a10usize;
        let answers_i4 = 0x4a20usize;

        record_coroutine_door_if_the_signature_says_so("MoveNext", 0, Some(Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN), false, door);
        record_coroutine_door_if_the_signature_says_so("MoveNext", 0, Some(Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN), true, static_sibling);
        record_coroutine_door_if_the_signature_says_so("MoveNext", 0, Some(Il2CppTypeEnum_IL2CPP_TYPE_I4), false, answers_i4);

        assert!(is_coroutine_door_target(door), "the door the dump lists for every enumerator class was not recorded as one");
        assert!(!is_coroutine_door_target(static_sibling), "a static MoveNext was treated as a coroutine door");
        assert!(!is_coroutine_door_target(answers_i4), "a method that does not answer the coroutine's bool was treated as a door");

        // An address no walk of this crate ever published. A hook the interceptor cannot place here is
        // taken down where it was asked for, which is the contract every caller other than the door rule
        // is written against.
        assert!(!is_coroutine_door_target(0xb00usize));

        // 0 is what a lookup answers when it found nothing, and a hook is never installed on it
        // (AGENTS section 2), so it may not enter the record and make some later take-down wait for a
        // tick on a method that does not exist.
        record_coroutine_door_target(0);
        assert!(!is_coroutine_door_target(0));

        // Recording the same door again - the next coroutine of the same class resolving it a second
        // time - is the same door, and the walk that publishes it is cached per class anyway.
        record_coroutine_door_if_the_signature_says_so("MoveNext", 0, Some(Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN), false, door);
        assert!(is_coroutine_door_target(door));
    }
}