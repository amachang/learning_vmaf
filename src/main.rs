use std::{env, process, ptr, ptr::NonNull, mem::MaybeUninit, path::Path, ffi::CString, sync::{Arc, Mutex}};
use libvmaf_sys::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use gst::prelude::*;
use os_str_bytes::OsStrBytes;

const WIDTH: usize = 960;
const HEIGHT: usize = 540;

struct ShareableVmafContext {
    ptr: NonNull<VmafContext>,
}

impl ShareableVmafContext {
    fn new(ptr: *mut VmafContext) -> Self {
        Self { ptr: unsafe { NonNull::new_unchecked(ptr) } }
    }

    fn as_ptr(&self) -> *mut VmafContext {
        self.ptr.as_ptr()
    }
}

unsafe impl Send for ShareableVmafContext { }

fn main() {
    unsafe { unsafe_main() }
}

unsafe fn unsafe_main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: {} <input video path> <output path> <video encoder> <audio encoder>", args[0]);
        process::exit(1);
    }
    let input_path = &args[1];

    gst::init().expect("Failed to gstreamer initialization");

    let vmaf_conf: VmafConfiguration = MaybeUninit::zeroed().assume_init();
    let mut vmaf_ctx: *mut VmafContext = ptr::null_mut();
    let vmaf_ctx_ptr: *mut *mut VmafContext = &mut vmaf_ctx;
    vmaf_init(vmaf_ctx_ptr, vmaf_conf);

    let mut vmaf_model_conf: VmafModelConfig = MaybeUninit::zeroed().assume_init();
    let vmaf_model_conf_ptr: *mut VmafModelConfig = &mut vmaf_model_conf;
    let mut vmaf_model: *mut VmafModel = ptr::null_mut();
    let vmaf_model_ptr: *mut *mut VmafModel = &mut vmaf_model;

    let model_path = Path::new("todo");
    let model_path_cstr = CString::new(model_path.as_os_str().to_raw_bytes()).unwrap();
    vmaf_model_load_from_path(vmaf_model_ptr, vmaf_model_conf_ptr, model_path_cstr.as_ptr());

    vmaf_use_features_from_model(vmaf_ctx, vmaf_model);

    let pipeline_def = format!("filesrc location={} ! decodebin ! videoconvert ! videoscale ! video/x-raw,format=I420 ! appsink name=out", input_path);
    let pipeline = gst::parse_launch(&pipeline_def).expect("Failed pipeline parse");
    let appsink = pipeline.dynamic_cast_ref::<gst::Bin>().expect("Couldn't cast pipeline to bin")
        .by_name("out").expect("Couldn't get AppSink element")
        .dynamic_cast::<gst_app::AppSink>().expect("Couldn't cast AppSink element");

    let vmaf_ctx = Arc::new(Mutex::new(ShareableVmafContext::new(vmaf_ctx)));
    let vmaf_ctx_weak = Arc::downgrade(&vmaf_ctx);

    let count = Arc::new(Mutex::new(0));
    let count_weak = Arc::downgrade(&count);

    let callbacks = gst_app::app_sink::AppSinkCallbacks::builder().new_sample(move |appsink| {
        let (Some(vmaf_ctx), Some(count)) = (vmaf_ctx_weak.upgrade(), count_weak.upgrade()) else {
            return Err(gst::FlowError::CustomError);
        };

        let sample = appsink.pull_sample().expect("Failed to pull sample");
        let buffer = sample.buffer().expect("Failed to get buffer from sample");
        let map = buffer.map_readable().expect("Failed to get readable map from buffer");

        let mut pic_ref: VmafPicture = MaybeUninit::zeroed().assume_init();
        let pic_ref_ptr: *mut VmafPicture = &mut pic_ref;
        let mut pic_dist: VmafPicture = MaybeUninit::zeroed().assume_init();
        let pic_dist_ptr: *mut VmafPicture = &mut pic_dist;
        vmaf_picture_alloc(pic_ref_ptr, VmafPixelFormat::VMAF_PIX_FMT_YUV420P, 8, WIDTH as u32, HEIGHT as u32);
        vmaf_picture_alloc(pic_dist_ptr, VmafPixelFormat::VMAF_PIX_FMT_YUV420P, 8, WIDTH as u32, HEIGHT as u32);

        let res: usize = WIDTH * HEIGHT;

        let data = map.as_slice();

        // 一旦同じ画像を比較するだけ
        // ここでちゃんと動いたら tee してやるサンプルも作る

        pic_ref.data[0] = data[0..res].as_ptr() as *mut _;
        pic_ref.data[1] = data[res..res*2].as_ptr() as *mut _;
        pic_ref.data[2] = data[res*2..res*3].as_ptr() as *mut _;
        pic_dist.data[0] = data[0..res].as_ptr() as *mut _;
        pic_dist.data[1] = data[res..res*2].as_ptr() as *mut _;
        pic_dist.data[2] = data[res*2..res*3].as_ptr() as *mut _;
        {
            let vmaf_ctx = vmaf_ctx.lock().unwrap();
            let mut count = count.lock().unwrap();

            vmaf_read_pictures(vmaf_ctx.as_ptr(), pic_ref_ptr, pic_dist_ptr, *count);
            *count += 1;
        }

        // Free resources
        vmaf_picture_unref(pic_ref_ptr);
        vmaf_picture_unref(pic_dist_ptr);

        Ok(gst::FlowSuccess::Ok)
    }).build();
    appsink.set_callbacks(callbacks);

    pipeline.set_state(gst::State::Playing).expect("Failed to play");
    let bus = pipeline.bus().expect("Failed to get bus");

    for msg in bus.iter_timed(gstreamer::ClockTime::NONE) {
        match msg.view() {
            gstreamer::MessageView::Eos(..) => break,
            gstreamer::MessageView::Error(err) => {
                eprintln!("Error from {:?}: {} ({:?})", msg.src().map(|s| s.path_string()), err.error(), err.debug());
                process::exit(1);
            }
            _ => (),
        }
    }

    pipeline.set_state(gst::State::Null).expect("Failed to set pipeline state null");

    {
        let vmaf_ctx = vmaf_ctx.lock().unwrap();
        let count = count.lock().unwrap();

        let mut score: f64 = 0.0f64;
        let score_ptr: *mut f64 = &mut score;
        vmaf_score_pooled(
            vmaf_ctx.as_ptr(), vmaf_model, VmafPoolingMethod::VMAF_POOL_METHOD_HARMONIC_MEAN, score_ptr, 0, *count);
        vmaf_close(vmaf_ctx.as_ptr());
        println!("VMAF = {}", score);
    }
}

