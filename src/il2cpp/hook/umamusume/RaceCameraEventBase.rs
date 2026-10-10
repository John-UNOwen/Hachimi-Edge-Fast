use crate::{
    windows::free_camera::{self, CameraScene},
    il2cpp::{
        symbols::get_method_addr,
        types::*,
    },
};

type CameraGetFloatFn = extern "C" fn(this: *mut Il2CppObject) -> f32;
def_detour! {
    get_CameraFov(this: *mut Il2CppObject) -> f32 {
            if let Some(fov) = free_camera::fov_for_scene(CameraScene::Race) {
            return fov;
        }
        get_orig_fn!(get_CameraFov, CameraGetFloatFn)(this)
    }
    // A fov of 0 is a projection the game cannot build. The free camera half runs before the game
    // is asked, so on a trip its own answer is still in front of the wrapper and answering with it
    // replays nothing.
    bail {
                get_orig_fn!(get_CameraFov, CameraGetFloatFn)(this)
    }
}

pub fn init(umamusume: *const Il2CppImage) {
    get_class_or_return!(umamusume, Gallop, RaceCameraEventBase);

    let get_CameraFov_addr = get_method_addr(RaceCameraEventBase, c"get_CameraFov", 0);
    new_hook!(get_CameraFov_addr, get_CameraFov);
}
