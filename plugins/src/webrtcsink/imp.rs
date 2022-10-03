use anyhow::Context;
use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gst_rtp::prelude::*;
use gst_utils::StreamProducer;
use gst_video::subclass::prelude::*;
use gst_webrtc::WebRTCDataChannel;

use async_std::task;
use futures::prelude::*;

use anyhow::{anyhow, Error};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;

use super::WebRTCSinkError;
use crate::signaller::Signaller;
use std::collections::BTreeMap;

static CAT: Lazy<gst::DebugCategory> = Lazy::new(|| {
    gst::DebugCategory::new(
        "webrtcsink",
        gst::DebugColorFlags::empty(),
        Some("WebRTC sink"),
    )
});

const CUDA_MEMORY_FEATURE: &str = "memory:CUDAMemory";
const GL_MEMORY_FEATURE: &str = "memory:GLMemory";
const NVMM_MEMORY_FEATURE: &str = "memory:NVMM";

const RTP_TWCC_URI: &str =
    "http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01";

const DEFAULT_STUN_SERVER: Option<&str> = Some("stun://stun.l.google.com:19302");
const DEFAULT_DISPLAY_NAME: Option<&str> = None;
const DEFAULT_ENABLE_DATA_CHANNEL_NAVIGATION: bool = false;

const DEFAULT_BITRATE: u32 = 2048000;

/// User configuration
struct Settings {
    video_caps: gst::Caps,
    audio_caps: gst::Caps,
    turn_server: Option<String>,
    stun_server: Option<String>,
    bitrate: u32,
    enable_data_channel_navigation: bool,
    display_name: Option<String>,
}

/// Represents a codec we can offer
#[derive(Debug)]
struct Codec {
    encoder: gst::ElementFactory,
    payloader: gst::ElementFactory,
    caps: gst::Caps,
    _payload: i32,
}

impl Codec {
    fn is_video(&self) -> bool {
        self.encoder
            .has_type(gst::ElementFactoryType::VIDEO_ENCODER)
    }
}

/// Wrapper around our sink pads
#[derive(Debug, Clone)]
struct InputStream {
    sink_pad: gst::GhostPad,
    producer: Option<StreamProducer>,
    /// The (fixed) caps coming in
    in_caps: Option<gst::Caps>,
    /// Pace input data
    clocksync: Option<gst::Element>,
    // Payload according being video or audio
    payload: Option<i32>,
    // Tee associated in each stream, to connect all consumers
    tee: Option<gst::Element>,
    /// Saves ssrc for all the consumers
    ssrc: u32,
}

/// Wrapper around webrtcbin pads
#[derive(Clone)]
struct WebRTCPad {
    pad: gst::Pad,
    // The m= line index in the SDP
    media_idx: u32,
    // The name of the corresponding InputStream's sink_pad
    stream_name: String,
}

struct Consumer {
    webrtcbin: gst::Element,
    webrtc_pads: HashMap<u32, WebRTCPad>,
    peer_id: String,
    sdp: Option<gst_sdp::SDPMessage>,
}

#[derive(PartialEq)]
enum SignallerState {
    Started,
    Stopped,
}

#[derive(Debug, serde::Deserialize)]
struct NavigationEvent {
    mid: Option<String>,
    #[serde(flatten)]
    event: gst_video::NavigationEvent,
}

/* Our internal state */
struct State {
    signaller: Box<dyn super::SignallableObject>,
    signaller_state: SignallerState,
    consumers: HashMap<String, Consumer>,
    codecs: BTreeMap<String, Codec>,
    pipeline_prepared: bool,
    audio_serial: u32,
    video_serial: u32,
    streams: HashMap<String, InputStream>,
    navigation_handler: Option<NavigationEventHandler>,
    mids: HashMap<String, String>,
    pipeline: gst::Pipeline,
    links: HashMap<String, gst_utils::ConsumptionLink>,
}

fn create_navigation_event(sink: &super::WebRTCSink, msg: &str) {
    let event: Result<NavigationEvent, _> = serde_json::from_str(msg);

    if let Ok(event) = event {
        gst::log!(CAT, obj: sink, "Processing navigation event: {:?}", event);

        if let Some(mid) = event.mid {
            let this = WebRTCSink::from_instance(sink);

            let state = this.state.lock().unwrap();
            if let Some(stream_name) = state.mids.get(&mid) {
                if let Some(stream) = state.streams.get(stream_name) {
                    let event = gst::event::Navigation::new(event.event.structure());

                    if !stream.sink_pad.push_event(event.clone()) {
                        gst::info!(CAT, "Could not send event: {:?}", event);
                    }
                }
            }
        } else {
            let this = WebRTCSink::from_instance(sink);

            let state = this.state.lock().unwrap();
            let event = gst::event::Navigation::new(event.event.structure());
            state.streams.iter().for_each(|(_, stream)| {
                if stream.sink_pad.name().starts_with("video_") {
                    gst::log!(CAT, "Navigating to: {:?}", event);
                    if !stream.sink_pad.push_event(event.clone()) {
                        gst::info!(CAT, "Could not send event: {:?}", event);
                    }
                }
            });
        }
    } else {
        gst::error!(CAT, "Invalid navigation event: {:?}", msg);
    }
}

/// Wrapper around `gst::ElementFactory::make` with a better error
/// message
pub fn make_element(element: &str, name: Option<&str>) -> Result<gst::Element, Error> {
    gst::ElementFactory::make(element, name)
        .with_context(|| format!("Failed to make element {}", element))
}

/// Simple utility for tearing down a pipeline cleanly
struct PipelineWrapper(gst::Pipeline);

// Structure to generate GstNavigation event from a WebRTCDataChannel
// This is simply used to hold references to the inner items.
#[derive(Debug)]
struct NavigationEventHandler((glib::SignalHandlerId, WebRTCDataChannel));

/// Our instance structure
#[derive(Default)]
pub struct WebRTCSink {
    state: Mutex<State>,
    settings: Mutex<Settings>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            video_caps: gst::Caps::new_empty(),
            audio_caps: gst::Caps::new_empty(),
            stun_server: DEFAULT_STUN_SERVER.map(String::from),
            turn_server: None,
            bitrate: DEFAULT_BITRATE,
            enable_data_channel_navigation: DEFAULT_ENABLE_DATA_CHANNEL_NAVIGATION,
            display_name: DEFAULT_DISPLAY_NAME.map(String::from),
        }
    }
}

impl Default for State {
    fn default() -> Self {
        let signaller = Signaller::default();
        
        let pipeline = gst::Pipeline::new(None);

        Self {
            signaller: Box::new(signaller),
            signaller_state: SignallerState::Stopped,
            consumers: HashMap::new(),
            codecs: BTreeMap::new(),
            pipeline_prepared: false,
            audio_serial: 0,
            video_serial: 0,
            streams: HashMap::new(),
            navigation_handler: None,
            mids: HashMap::new(),
            pipeline,
            links: HashMap::new()
        }
    }
}

fn make_converter_for_video_caps(caps: &gst::Caps) -> Result<gst::Element, Error> {
    assert!(caps.is_fixed());

    let video_info = gst_video::VideoInfo::from_caps(&caps)?;

    let ret = gst::Bin::new(None);

    let (head, mut tail) = {
        if let Some(feature) = caps.features(0) {
            if feature.contains(CUDA_MEMORY_FEATURE) {
                let cudaupload = make_element("cudaupload", None)?;
                let cudaconvert = make_element("cudaconvert", None)?;
                let cudascale = make_element("cudascale", None)?;

                ret.add_many(&[&cudaupload, &cudaconvert, &cudascale])?;
                gst::Element::link_many(&[&cudaupload, &cudaconvert, &cudascale])?;

                (cudaupload, cudascale)
            } else if feature.contains(GL_MEMORY_FEATURE) {
                let glupload = make_element("glupload", None)?;
                let glconvert = make_element("glcolorconvert", None)?;
                let glscale = make_element("glcolorscale", None)?;

                ret.add_many(&[&glupload, &glconvert, &glscale])?;
                gst::Element::link_many(&[&glupload, &glconvert, &glscale])?;

                (glupload, glscale)
            } else if feature.contains(NVMM_MEMORY_FEATURE) {
                let queue = make_element("queue", None)?;
                let nvconvert = make_element("nvvideoconvert", None)?;
                nvconvert.set_property_from_str("compute-hw", "VIC");
                nvconvert.set_property_from_str("nvbuf-memory-type", "nvbuf-mem-surface-array");

                ret.add_many(&[&queue, &nvconvert])?;
                gst::Element::link_many(&[&queue, &nvconvert])?;

                (queue, nvconvert)
            } else {
                let convert = make_element("videoconvert", None)?;
                let scale = make_element("videoscale", None)?;

                ret.add_many(&[&convert, &scale])?;
                gst::Element::link_many(&[&convert, &scale])?;

                (convert, scale)
            }
        } else {
            let convert = make_element("videoconvert", None)?;
            let scale = make_element("videoscale", None)?;

            ret.add_many(&[&convert, &scale])?;
            gst::Element::link_many(&[&convert, &scale])?;

            (convert, scale)
        }
    };

    ret.add_pad(
        &gst::GhostPad::with_target(Some("sink"), &head.static_pad("sink").unwrap()).unwrap(),
    )
    .unwrap();

    if video_info.fps().numer() != 0 {
        let vrate = make_element("videorate", None)?;
        vrate.set_property("drop-only", true);
        vrate.set_property("skip-to-first", true);

        ret.add(&vrate)?;
        tail.link(&vrate)?;
        tail = vrate;
    }

    ret.add_pad(
        &gst::GhostPad::with_target(Some("src"), &tail.static_pad("src").unwrap()).unwrap(),
    )
    .unwrap();

    Ok(ret.upcast())
}

/// Default configuration for known encoders, can be disabled
/// by returning True from an encoder-setup handler.
fn configure_encoder(enc: &gst::Element, bitrate: u32) {
    if let Some(factory) = enc.factory() {
        match factory.name().as_str() {
            "vp8enc" | "vp9enc" => {
                enc.set_property("deadline", 1i64);
                enc.set_property("target-bitrate", bitrate as i32);
                enc.set_property("cpu-used", -16i32);
                enc.set_property("keyframe-max-dist", 2000i32);
                enc.set_property_from_str("keyframe-mode", "disabled");
                enc.set_property_from_str("end-usage", "cbr");
                enc.set_property("buffer-initial-size", 100i32);
                enc.set_property("buffer-optimal-size", 120i32);
                enc.set_property("buffer-size", 150i32);
                enc.set_property("max-intra-bitrate", 250i32);
                enc.set_property_from_str("error-resilient", "default");
                enc.set_property("lag-in-frames", 0i32);
            }
            "x264enc" => {
                enc.set_property("bitrate", bitrate / 1000);
                enc.set_property_from_str("tune", "zerolatency");
                enc.set_property_from_str("speed-preset", "ultrafast");
                enc.set_property("threads", 12u32);
                enc.set_property("key-int-max", 2560u32);
                enc.set_property("b-adapt", false);
                enc.set_property("vbv-buf-capacity", 120u32);
            }
            "nvh264enc" => {
                enc.set_property("bitrate", bitrate / 1000);
                enc.set_property("gop-size", 2560i32);
                enc.set_property_from_str("rc-mode", "cbr-ld-hq");
                enc.set_property("zerolatency", true);
            }
            "vaapih264enc" | "vaapivp8enc" => {
                enc.set_property("bitrate", bitrate / 1000);
                enc.set_property("keyframe-period", 2560u32);
                enc.set_property_from_str("rate-control", "cbr");
            }
            "nvv4l2h264enc" => {
                enc.set_property("bitrate", bitrate);
                enc.set_property("insert-sps-pps", true);
                enc.set_property_from_str("preset-level", "UltraFastPreset");
                enc.set_property("maxperf-enable", true);
                enc.set_property("insert-vui", true);
                enc.set_property("idrinterval", 256u32);
                enc.set_property("insert-aud", true);
                enc.set_property_from_str("control-rate", "variable_bitrate");
            }
            "nvv4l2vp8enc" => {
                enc.set_property("bitrate", bitrate);
                enc.set_property_from_str("preset-level", "UltraFastPreset");
                enc.set_property("maxperf-enable", true);
                enc.set_property("idrinterval", 256u32);
                enc.set_property_from_str("control-rate", "variable_bitrate");
            }
            _ => (),
        }
    }
}

/// Bit of an awkward function, but the goal here is to keep
/// most of the encoding code for consumers in line with
/// the codec discovery code, and this gets the job done.
fn setup_encoding(
    pipeline: &gst::Pipeline,
    src: &gst::Element,
    input_caps: &gst::Caps,
    codec: &Codec,
    ssrc: Option<u32>,
    twcc: bool,
) -> Result<(gst::Element, gst::Element, gst::Element), Error> {
    let conv = match codec.is_video() {
        true => make_converter_for_video_caps(input_caps)?.upcast(),
        false => gst::parse_bin_from_description("audioresample ! audioconvert", true)?.upcast(),
    };

    let conv_filter = make_element("capsfilter", None)?;

    let queue = make_element("queue", None)?;

    let enc = codec
        .encoder
        .create(None)
        .with_context(|| format!("Creating encoder {}", codec.encoder.name()))?;
    let pay = codec
        .payloader
        .create(None)
        .with_context(|| format!("Creating payloader {}", codec.payloader.name()))?;
    let parse_filter = make_element("capsfilter", None)?;

    //pay.set_property("pt", codec.payload as u32);

    if let Some(ssrc) = ssrc {
        pay.set_property("ssrc", ssrc);
    }

    pipeline
        .add_many(&[&conv, &conv_filter, &queue, &enc, &parse_filter, &pay])
        .unwrap();
    gst::Element::link_many(&[src, &conv, &conv_filter, &queue, &enc])
        .with_context(|| "Linking encoding elements")?;

    let codec_name = codec.caps.structure(0).unwrap().name();

    if let Some(parser) = if codec_name == "video/x-h264" {
        Some(make_element("h264parse", None)?)
    } else if codec_name == "video/x-h265" {
        Some(make_element("h265parse", None)?)
    } else {
        None
    } {
        pipeline.add(&parser).unwrap();
        gst::Element::link_many(&[&enc, &parser, &parse_filter])
            .with_context(|| "Linking encoding elements")?;
    } else {
        gst::Element::link_many(&[&enc, &parse_filter])
            .with_context(|| "Linking encoding elements")?;
    }

    let conv_caps = if codec.is_video() {
        let mut structure_builder = gst::Structure::builder("video/x-raw")
            .field("pixel-aspect-ratio", gst::Fraction::new(1, 1));

        if codec.encoder.name() == "nvh264enc" {
            // Quirk: nvh264enc can perform conversion from RGB formats, but
            // doesn't advertise / negotiate colorimetry correctly, leading
            // to incorrect color display in Chrome (but interestingly not in
            // Firefox). In any case, restrict to exclude RGB formats altogether,
            // and let videoconvert do the conversion properly if needed.
            structure_builder =
                structure_builder.field("format", &gst::List::new(&[&"NV12", &"YV12", &"I420"]));
        }

        gst::Caps::builder_full_with_any_features()
            .structure(structure_builder.build())
            .build()
    } else {
        gst::Caps::builder("audio/x-raw").build()
    };

    match codec.encoder.name().as_str() {
        "vp8enc" | "vp9enc" => {
            pay.set_property_from_str("picture-id-mode", "15-bit");
        }
        _ => (),
    }

    /* We only enforce TWCC in the offer caps, once a remote description
     * has been set it will get automatically negotiated. This is necessary
     * because the implementor in Firefox had apparently not understood the
     * concept of *transport-wide* congestion control, and firefox doesn't
     * provide feedback for audio packets.
     */
    if twcc {
        let twcc_extension = gst_rtp::RTPHeaderExtension::create_from_uri(RTP_TWCC_URI).unwrap();
        twcc_extension.set_id(1);
        pay.emit_by_name::<()>("add-extension", &[&twcc_extension]);
    }

    conv_filter.set_property("caps", conv_caps);

    let parse_caps = if codec_name == "video/x-h264" {
        gst::Caps::builder(codec_name)
            .field("stream-format", "avc")
            .field("profile", "constrained-baseline")
            .build()
    } else if codec_name == "video/x-h265" {
        gst::Caps::builder(codec_name)
            .field("stream-format", "hvc1")
            .build()
    } else {
        gst::Caps::new_any()
    };

    parse_filter.set_property("caps", parse_caps);

    gst::Element::link_many(&[&parse_filter, &pay]).with_context(|| "Linking encoding elements")?;

    Ok((enc, conv_filter, pay))
}

impl State {

    fn generate_ssrc(&self) -> u32 {

        loop {
            let ret = fastrand::u32(..);
            if let Some(_result) = self.streams.iter().find(|(_,stream)| stream.ssrc == ret) {
                continue;
            }
            return ret;
        }

    }

    fn finalize_consumer(
        &mut self,
        element: &super::WebRTCSink,
        consumer: &mut Consumer,
        signal: bool,
    ) {
        gst::info!(CAT, "Removing: {}", consumer.webrtcbin.name());

        let mut it = consumer.webrtc_pads.iter().peekable();

        while let Some((_, webrtc_pad)) = it.next()  {
            
            if let Some(queue_src_pad) = webrtc_pad.pad.peer(){
                
                let queue = queue_src_pad.parent_element().unwrap();
                let queue_sink_pad = queue.static_pad("sink").unwrap();

                if let Some(tee_src_pad) = queue_sink_pad.peer(){                
                    let tee = tee_src_pad.parent_element().unwrap();
                    let tee_block = tee_src_pad
                        .add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, |_pad, _info| {
                            gst::PadProbeReturn::Ok
                        })
                        .unwrap();
                    
                    if queue_src_pad.unlink(&webrtc_pad.pad).is_err() {
                        gst::info!(
                            CAT,
                            "ERROR, Failed to unlink queue src to webrtc pad"
                        );
                    }
                    if tee_src_pad.unlink(&queue_sink_pad).is_err() {
                        gst::info!(
                            CAT,
                            "ERROR, Failed to unlink tee src to queue sink"
                        );                    
                    }
                    tee_src_pad.remove_probe(tee_block);
                    tee.release_request_pad(&tee_src_pad);
                    consumer.webrtcbin.release_request_pad(&webrtc_pad.pad);

                    if queue.set_state(gst::State::Null).is_err() {
                        gst::info!(
                            CAT,
                            "ERROR, Failed to set queue to Null"
                        );
                    }

                    if  self.pipeline.remove(&queue).is_err() {
                        gst::info!(
                            CAT,
                            "ERROR, Failed to remove queue"
                        );
                    }

                }
                else {
                    gst::debug!(
                        CAT,
                        obj: element,
                        "Queue pad {} is not linked to tee pad",
                        webrtc_pad.pad.name()
                    )
                }

            }
            else {
                gst::debug!(
                    CAT,
                    obj: element,
                    "Webrtc pad {} is not linked to queue pad",
                    webrtc_pad.pad.name()
                );
            }

        }

        let test = self.pipeline.downgrade();
        consumer.webrtcbin.call_async( move |webrtcbin| {
            let pipeline = test.upgrade().unwrap();
            
            if webrtcbin.set_state(gst::State::Null).is_err() {
                gst::info!(
                    CAT,
                    "Failed to set webrtcbin to Null"
                );
            }

            if  pipeline.remove(webrtcbin).is_err() {
                gst::info!(
                    CAT,
                    "ERROR, Failed to remove webrtcbin"
                );
            }
        });

        if signal {
            self.signaller.consumer_removed(element, &consumer.peer_id);
            
        }
    }

    fn remove_consumer(
        &mut self,
        element: &super::WebRTCSink,
        peer_id: &str,
        signal: bool,
    ) -> Option<Consumer> {
        if let Some(mut consumer) = self.consumers.remove(peer_id) {
            self.finalize_consumer(element, &mut consumer, signal);
            Some(consumer)
        } else {
            None
        }
    }

    fn maybe_start_signaller(&mut self, element: &super::WebRTCSink) {
        if self.signaller_state == SignallerState::Stopped
            && element.current_state() >= gst::State::Paused
            && self.pipeline_prepared
        {
            if let Err(err) = self.signaller.start(element) {
                gst::error!(CAT, obj: element, "error: {}", err);
                gst::element_error!(
                    element,
                    gst::StreamError::Failed,
                    ["Failed to start signaller {}", err]
                );
            } else {
                gst::info!(CAT, "Started signaller");
                self.signaller_state = SignallerState::Started;
            }
        }
    }

    fn maybe_stop_signaller(&mut self, element: &super::WebRTCSink) {
        if self.signaller_state == SignallerState::Started {
            self.signaller.stop(element);
            self.signaller_state = SignallerState::Stopped;
            gst::info!(CAT, "Stopped signaller");
        }
    }
}

impl Consumer {
    fn new(
        webrtcbin: gst::Element,
        peer_id: String,
    ) -> Self {
        Self {
            webrtcbin,
            peer_id,
            sdp: None,
            webrtc_pads: HashMap::new(),
        }
    }

    /// Request a sink pad on our webrtcbin, and set its transceiver's codec_preferences
    fn request_webrtcbin_pad(
        &mut self,
        _element: &super::WebRTCSink,
        stream: &InputStream,
    ) {
        let media_idx = self.webrtc_pads.len() as i32;

        let pad = self
            .webrtcbin
            .request_pad_simple(&format!("sink_{}", media_idx))
            .unwrap();

        self.webrtc_pads.insert(
            stream.ssrc,
            WebRTCPad {
                pad,
                media_idx: media_idx as u32,
                stream_name: stream.sink_pad.name().to_string(),
            },
        );
    }

    /// Called when we have received an answer, connects an InputStream
    /// to a given WebRTCPad
    fn connect_input_stream(
        &mut self,
        element: &super::WebRTCSink,
        webrtc_pad: &WebRTCPad,
        stream: &InputStream,
        pipeline: &gst::Pipeline,
    ) -> Result<(), Error> {

        gst::info!(
            CAT,
            obj: element,
            "Connecting input stream {} for consumer {}",
            webrtc_pad.stream_name,
            self.peer_id
        );


        let pad_template = stream.tee.as_ref().unwrap().pad_template("src_%u").unwrap();
        let tee_pad = stream.tee.as_ref().unwrap().request_pad(&pad_template, None, None).unwrap();
        
        let tee_block = tee_pad
            .add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, |_pad, _info| {
                gst::PadProbeReturn::Ok
            })
            .unwrap();

        let queue = make_element("queue", None)?;
        pipeline.add(&queue).unwrap();
        let queue_src = queue.static_pad("src").unwrap();
        
        queue.sync_state_with_parent().unwrap();

        if self.webrtcbin.sync_state_with_parent().is_err() {
            gst::error!(
                CAT,
                obj: &self.webrtcbin,
                "Failed to set webrtcbin to Playing"
            );
        }

        stream.tee.as_ref().unwrap().link(&queue)?;

        queue_src.link(&webrtc_pad.pad)
            .with_context(|| format!("Connecting input stream for {}", self.peer_id))?;
        
        tee_pad.remove_probe(tee_block);
               
        Ok(())
    }
}

impl Drop for PipelineWrapper {
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}

impl InputStream {

    /// Called when transitioning state up to Paused
    fn prepare(&mut self, element: &super::WebRTCSink) -> Result<(), Error> {
        let clocksync = make_element("clocksync", None)?;
        let appsink = make_element("appsink", None)?
            .downcast::<gst_app::AppSink>()
            .unwrap();
        appsink.set_property("drop", true);
        appsink.set_property("emit-signals", true);
        element.add(&clocksync).unwrap();
        element.add(&appsink).unwrap();

        clocksync
            .link(&appsink)
            .with_context(|| format!("Linking input stream {}", self.sink_pad.name()))?;

        element
            .sync_children_states()
            .with_context(|| format!("Linking input stream {}", self.sink_pad.name()))?;

        self.sink_pad
            .set_target(Some(&clocksync.static_pad("sink").unwrap()))
            .unwrap();

        let producer = StreamProducer::from(&appsink);
        producer.forward();
        self.producer = Some(producer);

        Ok(())
    }

    /// Called when transitioning state back down to Ready
    fn unprepare(&mut self, element: &super::WebRTCSink) {
        self.sink_pad.set_target(None::<&gst::Pad>).unwrap();

        if let Some(clocksync) = self.clocksync.take() {
            element.remove(&clocksync).unwrap();
            clocksync.set_state(gst::State::Null).unwrap();
        }

        if let Some(producer) = self.producer.take() {
            let appsink = producer.appsink().upcast_ref::<gst::Element>();
            element.remove(appsink).unwrap();
            appsink.set_state(gst::State::Null).unwrap();
        }
    }

    fn create_caps_for_pay_filter(&self, codec_name: &str) -> gst::Caps{
        let payload = self.payload.unwrap();
        
        let mut struct_caps_pay = gst::Structure::builder("application/x-rtp")
            .field("payload", payload); 
        if self.sink_pad.name().starts_with("video_") {
            struct_caps_pay = struct_caps_pay.field("media", "video")
                .field("clock-rate", 90000 as i32);
            struct_caps_pay = match codec_name {
                "video/x-h265" => struct_caps_pay.field("encoding-name", "H265"),
                "video/x-h264" => struct_caps_pay.field("encoding-name", "H264"),
                "video/x-vp8" => struct_caps_pay.field("encoding-name", "VP8"),
                "video/x-vp9" => struct_caps_pay.field("encoding-name", "VP9"),
                _ => struct_caps_pay.field("encoding-name", "VP8"),
            };

        } else{
            struct_caps_pay = struct_caps_pay.field("media", "audio")
                .field("clock-rate", 48000 as i32)
                .field("encoding-name", "OPUS");
        }

        gst::Caps::builder_full().structure(struct_caps_pay.build()).build()

    }

    fn create_pipeline(&mut self, element: &super::WebRTCSink, pipeline: &gst::Pipeline, codecs: &BTreeMap<String, Codec>, links: &mut HashMap<String, gst_utils::ConsumptionLink>, stream_name: &String) -> Result<(), Error> {

        let get_codec = if stream_name.starts_with("video") {
            "video"
        } else {
            "audio"
        };

        let codec = codecs
            .get(get_codec)
            .ok_or_else(|| anyhow!("No codec for {}", stream_name))?;

        gst::info!(CAT, "ROXO {:?}", codec.caps);    

        let appsrc = make_element("appsrc", Some(&self.sink_pad.name()))?;
        pipeline.add(&appsrc).unwrap();

        let pay_filter = make_element("capsfilter", None)?;
        pipeline.add(&pay_filter).unwrap();

        let tee = make_element("tee", None).unwrap();
        pipeline.add(&tee).unwrap();

        let queue = make_element("queue", None)?;
        pipeline.add(&queue).unwrap();

        let fakesink = make_element("fakesink", None)?;
        fakesink.set_property("async", false);
        pipeline.add(&fakesink).unwrap();


        let (enc, _raw_filter, pay) = setup_encoding(
            &pipeline,
            &appsrc,
            self.in_caps.as_ref().unwrap(),
            codec,
            None,
            false,
        )?;

        element.emit_by_name::<bool>(
            "encoder-setup",
            &[&self.sink_pad.name(), &enc],
        );

        let caps = self.create_caps_for_pay_filter(codec.caps.structure(0).unwrap().name());
        pay_filter.set_property("caps", caps);

        let appsrc = appsrc.downcast::<gst_app::AppSrc>().unwrap();
        gst_utils::StreamProducer::configure_consumer(&appsrc);

        let result = match self.producer.as_ref().unwrap().add_consumer(&appsrc) {
            Ok(link) => {
                links.insert(self.sink_pad.name().to_string(), link);
                Ok(())
            }
            Err(err) => Err(anyhow!("Could not link producer: {:?}", err)),
        };

        let pad_template = tee.pad_template("src_%u").unwrap();

        tee.request_pad(&pad_template, None, None).unwrap();

        gst::Element::link_many(&[&pay, &pay_filter, &tee, &queue, &fakesink])
        .with_context(|| "Linking encoding elements")?;

        self.tee = Some(tee);

        result
    }

}

impl NavigationEventHandler {
    pub fn new(element: &super::WebRTCSink, webrtcbin: &gst::Element) -> Self {
        gst::info!(CAT, "Creating navigation data channel");
        let channel = webrtcbin.emit_by_name::<WebRTCDataChannel>(
            "create-data-channel",
            &[
                &"input",
                &gst::Structure::new(
                    "config",
                    &[("priority", &gst_webrtc::WebRTCPriorityType::High)],
                ),
            ],
        );

        let weak_element = element.downgrade();
        Self((
            channel.connect("on-message-string", false, move |values| {
                if let Some(element) = weak_element.upgrade() {
                    let _channel = values[0].get::<WebRTCDataChannel>().unwrap();
                    let msg = values[1].get::<&str>().unwrap();
                    create_navigation_event(&element, msg);
                }

                None
            }),
            channel,
        ))
    }
}

impl WebRTCSink {

    fn prepare_pipeline(&self, element: &super::WebRTCSink, state: &mut State) {
        let mut streams = state.streams.clone();
        streams.iter_mut().for_each(|(stream_name, stream )| {         
                if let Err(err) = stream.create_pipeline(&element, &state.pipeline, &state.codecs, &mut state.links, &stream_name) {
                    gst::error!(CAT, obj: element, "error creating main pipeline for producer: {}", err);
                    gst::element_error!(
                        element,
                        gst::StreamError::Failed,
                        ["Failed to start main pipeline for producer: {}", err]
                    );
                } else {
                    gst::info!(CAT, "Main pipeline for producer created")
                }
            }
        );

        state.streams = streams;

        let clock = element.clock();
        state.pipeline.use_clock(clock.as_ref());
        state.pipeline.set_start_time(gst::ClockTime::NONE);
        state.pipeline.set_base_time(element.base_time().unwrap());

        let mut bus_stream = state.pipeline.bus().unwrap().stream();
        let element_clone = element.downgrade();
        let pipeline_clone = state.pipeline.downgrade();

        task::spawn(async move {
            while let Some(msg) = bus_stream.next().await {
                if let Some(element) = element_clone.upgrade() {
                    let this = Self::from_instance(&element);
                    match msg.view() {
                        gst::MessageView::Error(err) => {
                            gst::error!(
                                CAT,
                                "Producer error: {}, details: {:?}",
                                err.error(),
                                err.debug()
                            );
                            let _ = this.unprepare(&element);
                        }
                        gst::MessageView::StateChanged(state_changed) => {
                            if let Some(pipeline) = pipeline_clone.upgrade() {
                                if Some(pipeline.clone().upcast()) == state_changed.src() {
                                    pipeline.debug_to_dot_file_with_ts(
                                        gst::DebugGraphDetails::all(),
                                        format!(
                                            "webrtcsink-producer-peer-{:?}-{:?}-to-{:?}",
                                            this.settings.lock().unwrap().display_name.clone().unwrap().to_string(),
                                            state_changed.old(),
                                            state_changed.current()
                                        ),
                                    );
                                }
                            }
                        }
                        gst::MessageView::Latency(..) => {
                            if let Some(pipeline) = pipeline_clone.upgrade() {
                                gst::info!(CAT, obj: &pipeline, "Recalculating latency");
                                let _ = pipeline.recalculate_latency();
                            }
                        }
                        gst::MessageView::Eos(..) => {
                            gst::error!(
                                CAT,
                                "Unexpected end of stream for producer",
                            );
                             let _ = this.unprepare(&element);
                        }
                        _ => (),
                    }
                }
            }
        });

       if state.pipeline.set_state(gst::State::Playing).map_err(|err| {
            WebRTCSinkError::ProducerPipelineError {
                details: err.to_string(),
            }
        }).is_err() {
            let _ = self.unprepare(element);
        }

    }

    fn get_codec_from_caps(&self, caps: &gst::Caps, payload: i32) -> Option<Codec>{

        /* First gather all encoder and payloader factories */
        let encoders = gst::ElementFactory::factories_with_type(
            gst::ElementFactoryType::ENCODER,
            gst::Rank::Marginal,
        );

        let payloaders = gst::ElementFactory::factories_with_type(
            gst::ElementFactoryType::PAYLOADER,
            gst::Rank::Marginal,
        );
        Option::zip(
            encoders
                .iter()
                .find(|factory| factory.can_src_any_caps(caps)),
            payloaders
                .iter()
                .find(|factory| factory.can_sink_any_caps(caps)),
        )
        .and_then(|(encoder, payloader)| {
            Some(Codec {
                encoder: encoder.clone(),
                payloader: payloader.clone(),
                caps: caps.to_owned(),
                _payload: payload,
            })
            
        })
    }

    /// Build an ordered map of Codecs, given user-provided audio / video caps */
    fn lookup_codecs(&self, element: &super::WebRTCSink,) -> BTreeMap<String, Codec> {

        let settings = self.settings.lock().unwrap();

        if settings.video_caps.is_empty() && settings.audio_caps.is_empty() {
            gst::error!(CAT, obj: element, "No video caps or audio were given!");

            gst::element_error!(
                element,
                gst::StreamError::Failed,
                ["No video caps or audio were given!"]
            );
        }

        let mut codecs: BTreeMap<String, Codec>  = BTreeMap::new();

        if !settings.video_caps.is_empty() {
            codecs.insert("video".to_string(), self.get_codec_from_caps(&settings.video_caps, 96).unwrap());
        }

        if !settings.audio_caps.is_empty() {
            codecs.insert("audio".to_string(), self.get_codec_from_caps(&settings.audio_caps, 97).unwrap());
        }

        codecs
  
        
    }

    /// Prepare for accepting consumers, by setting
    /// up StreamProducers for each of our sink pads
    fn prepare(&self, element: &super::WebRTCSink) -> Result<(), Error> {
        gst::debug!(CAT, obj: element, "preparing");

        self.state
            .lock()
            .unwrap()
            .streams
            .iter_mut()
            .try_for_each(|(_, stream)| stream.prepare(element))?;

        Ok(())
    }

    /// Unprepare by stopping consumers, then the signaller object.
    /// Might abort codec discovery
    fn unprepare(&self, element: &super::WebRTCSink) -> Result<(), Error> {
        gst::info!(CAT, obj: element, "unpreparing");

        let mut state = self.state.lock().unwrap();

        let consumer_ids: Vec<_> = state.consumers.keys().map(|k| k.to_owned()).collect();

        for id in consumer_ids {
            state.remove_consumer(element, &id, true);
        }

        state
            .streams
            .iter_mut()
            .for_each(|(_, stream)| stream.unprepare(element));

        state.links.clear();

        state.pipeline.set_state(gst::State::Null)?;

        state.maybe_stop_signaller(element);

        state.pipeline_prepared = false;
        state.codecs = BTreeMap::new();

        Ok(())
    }

    /// When using a custom signaller
    pub fn set_signaller(&self, signaller: Box<dyn super::SignallableObject>) -> Result<(), Error> {
        let mut state = self.state.lock().unwrap();

        state.signaller = signaller;

        Ok(())
    }

    /// Called by the signaller when it has encountered an error
    pub fn handle_signalling_error(&self, element: &super::WebRTCSink, error: anyhow::Error) {
        gst::error!(CAT, obj: element, "Signalling error: {:?}", error);

        gst::element_error!(
            element,
            gst::StreamError::Failed,
            ["Signalling error: {:?}", error]
        );
    }

    fn on_offer_created(
        &self,
        element: &super::WebRTCSink,
        offer: gst_webrtc::WebRTCSessionDescription,
        peer_id: &str,
    ) {
        let mut state = self.state.lock().unwrap();

        if let Some(consumer) = state.consumers.get(peer_id) {
            consumer
                .webrtcbin
                .emit_by_name::<()>("set-local-description", &[&offer, &None::<gst::Promise>]);

            if let Err(err) = state.signaller.handle_sdp(element, peer_id, &offer) {
                gst::warning!(
                    CAT,
                    "Failed to handle SDP for consumer {}: {}",
                    peer_id,
                    err
                );

                state.remove_consumer(element, peer_id, true);
            }
        }
    }

    fn negotiate(&self, element: &super::WebRTCSink, peer_id: &str) {
        let state = self.state.lock().unwrap();

        gst::debug!(CAT, obj: element, "Negotiating for peer {}", peer_id);

        if let Some(consumer) = state.consumers.get(peer_id) {
            let element = element.downgrade();
            gst::debug!(CAT, "Creating offer for peer {}", peer_id);
            let peer_id = peer_id.to_string();
            let promise = gst::Promise::with_change_func(move |reply| {
                gst::debug!(CAT, "Created offer for peer {}", peer_id);

                if let Some(element) = element.upgrade() {
                    let this = Self::from_instance(&element);
                    let reply = match reply {
                        Ok(Some(reply)) => reply,
                        Ok(None) => {
                            gst::warning!(
                                CAT,
                                obj: &element,
                                "Promise returned without a reply for {}",
                                peer_id
                            );

                            let _ = this.remove_consumer(&element, &peer_id, true);
                            return;
                        }
                        Err(err) => {
                            gst::warning!(
                                CAT,
                                obj: &element,
                                "Promise returned with an error for {}: {:?}",
                                peer_id,
                                err
                            );
                            let _ = this.remove_consumer(&element, &peer_id, true);
                            return;
                        }
                    };

                    if let Ok(offer) = reply
                        .value("offer")
                        .map(|offer| offer.get::<gst_webrtc::WebRTCSessionDescription>().unwrap())
                    {
                        this.on_offer_created(&element, offer, &peer_id);
                    } else {
                        gst::warning!(
                            CAT,
                            "Reply without an offer for consumer {}: {:?}",
                            peer_id,
                            reply
                        );
                        let _ = this.remove_consumer(&element, &peer_id, true);
                    }
                }
            });

            consumer
                .webrtcbin
                .emit_by_name::<()>("create-offer", &[&None::<gst::Structure>, &promise]);
        } else {
            gst::debug!(
                CAT,
                obj: element,
                "consumer for peer {} no longer exists",
                peer_id
            );
        }
    }

    fn on_ice_candidate(
        &self,
        element: &super::WebRTCSink,
        peer_id: String,
        sdp_m_line_index: u32,
        candidate: String,
    ) {
        let mut state = self.state.lock().unwrap();
        if let Err(err) =
            state
                .signaller
                .handle_ice(element, &peer_id, &candidate, Some(sdp_m_line_index), None)
        {
            gst::warning!(
                CAT,
                "Failed to handle ICE for consumer {}: {}",
                peer_id,
                err
            );

            state.remove_consumer(element, &peer_id, true);
        }
    }

    /// Called by the signaller to add a new consumer
    pub fn add_consumer(
        &self,
        element: &super::WebRTCSink,
        peer_id: &str,
    ) -> Result<(), WebRTCSinkError> {
        let settings = self.settings.lock().unwrap();
        let mut state = self.state.lock().unwrap();
        
        if state.consumers.contains_key(peer_id) {
            return Err(WebRTCSinkError::DuplicateConsumerId(peer_id.to_string()));
        }

        gst::info!(CAT, obj: element, "Adding consumer {}", peer_id);

        let webrtcbin = make_element("webrtcbin", None).map_err(|err| {
            WebRTCSinkError::ConsumerPipelineError {
                peer_id: peer_id.to_string(),
                details: err.to_string(),
            }
        })?;

        webrtcbin.set_property_from_str("bundle-policy", "max-compat");

        if let Some(stun_server) = settings.stun_server.as_ref() {
            webrtcbin.set_property("stun-server", stun_server);
        }

        if let Some(turn_server) = settings.turn_server.as_ref() {
            webrtcbin.set_property("turn-server", turn_server);
        }

        state.pipeline.add(&webrtcbin).unwrap();

        let element_clone = element.downgrade();
        let peer_id_clone = peer_id.to_owned();
        webrtcbin.connect("on-ice-candidate", false, move |values| {
            if let Some(element) = element_clone.upgrade() {
                let this = Self::from_instance(&element);
                let sdp_m_line_index = values[1].get::<u32>().expect("Invalid argument");
                let candidate = values[2].get::<String>().expect("Invalid argument");
                this.on_ice_candidate(
                    &element,
                    peer_id_clone.to_string(),
                    sdp_m_line_index,
                    candidate,
                );
            }
            None
        });

        let element_clone = element.downgrade();
        let peer_id_clone = peer_id.to_owned();
        webrtcbin.connect_notify(Some("connection-state"), move |webrtcbin, _pspec| {
            if let Some(element) = element_clone.upgrade() {
                let state =
                    webrtcbin.property::<gst_webrtc::WebRTCPeerConnectionState>("connection-state");

                match state {
                    gst_webrtc::WebRTCPeerConnectionState::Failed => {
                        let this = Self::from_instance(&element);
                        gst::warning!(
                            CAT,
                            obj: &element,
                            "Connection state for consumer {} failed",
                            peer_id_clone
                        );
                        let _ = this.remove_consumer(&element, &peer_id_clone, true);
                    }
                    _ => {
                        gst::log!(
                            CAT,
                            obj: &element,
                            "Connection state for consumer {} changed: {:?}",
                            peer_id_clone,
                            state
                        );
                    }
                }
            }
        });

        let element_clone = element.downgrade();
        let peer_id_clone = peer_id.to_owned();
        webrtcbin.connect_notify(Some("ice-connection-state"), move |webrtcbin, _pspec| {
            if let Some(element) = element_clone.upgrade() {
                let state = webrtcbin
                    .property::<gst_webrtc::WebRTCICEConnectionState>("ice-connection-state");
                let this = Self::from_instance(&element);

                match state {
                    gst_webrtc::WebRTCICEConnectionState::Failed => {
                        gst::warning!(
                            CAT,
                            obj: &element,
                            "Ice connection state for consumer {} failed",
                            peer_id_clone
                        );
                        let _ = this.remove_consumer(&element, &peer_id_clone, true);
                    }
                    _ => {
                        gst::log!(
                            CAT,
                            obj: &element,
                            "Ice connection state for consumer {} changed: {:?}",
                            peer_id_clone,
                            state
                        );
                    }
                }

                if state == gst_webrtc::WebRTCICEConnectionState::Completed {
                    let state = this.state.lock().unwrap();

                    if let Some(consumer) = state.consumers.get(&peer_id_clone) {
                        for webrtc_pad in consumer.webrtc_pads.values() {
                            if let Some(srcpad) = webrtc_pad.pad.peer() {
                                srcpad.send_event(
                                    gst_video::UpstreamForceKeyUnitEvent::builder()
                                        .all_headers(true)
                                        .build(),
                                );
                            }
                        }
                    }
                }
            }
        });

        let element_clone = element.downgrade();
        let peer_id_clone = peer_id.to_owned();
        webrtcbin.connect_notify(Some("ice-gathering-state"), move |webrtcbin, _pspec| {
            let state =
                webrtcbin.property::<gst_webrtc::WebRTCICEGatheringState>("ice-gathering-state");

            if let Some(element) = element_clone.upgrade() {
                gst::log!(
                    CAT,
                    obj: &element,
                    "Ice gathering state for consumer {} changed: {:?}",
                    peer_id_clone,
                    state
                );
            }
        });

        let mut consumer = Consumer::new(
            webrtcbin.clone(),
            peer_id.to_string()
        );

        state
            .streams
            .iter()
            .for_each(|(_, stream)| consumer.request_webrtcbin_pad(element, stream));


        if settings.enable_data_channel_navigation {
            state.navigation_handler = Some(NavigationEventHandler::new(element, &webrtcbin));
        }

        state.consumers.insert(peer_id.to_string(), consumer);

        drop(state);

        self.on_remote_description_set(&element, peer_id.to_string());

        // This is intentionally emitted with the pipeline in the Ready state,
        // so that application code can create data channels at the correct
        // moment.
        element.emit_by_name::<()>("consumer-added", &[&peer_id, &webrtcbin]);

        // We don't connect to on-negotiation-needed, this in order to call the above
        // signal without holding the state lock:
        //
        // Going to Ready triggers synchronous emission of the on-negotiation-needed
        // signal, during which time the application may add a data channel, causing
        // renegotiation, which we do not support at this time.
        //
        // This is completely safe, as we know that by now all conditions are gathered:
        // webrtcbin is in the Ready state, and all its transceivers have codec_preferences.
        self.negotiate(element, peer_id);

        Ok(())
    }

    /// Called by the signaller to remove a consumer
    pub fn remove_consumer(
        &self,
        element: &super::WebRTCSink,
        peer_id: &str,
        signal: bool,
    ) -> Result<(), WebRTCSinkError> {
        let mut state = self.state.lock().unwrap();

        if !state.consumers.contains_key(peer_id) {
            return Err(WebRTCSinkError::NoConsumerWithId(peer_id.to_string()));
        }

        if let Some(consumer) = state.remove_consumer(element, peer_id, signal) {
            drop(state);
            element.emit_by_name::<()>("consumer-removed", &[&peer_id, &consumer.webrtcbin]);
        }

        Ok(())
    }

    fn on_remote_description_set(&self, element: &super::WebRTCSink, peer_id: String) {
        let mut state = self.state.lock().unwrap();
        let mut remove = false;

        if let Some(mut consumer) = state.consumers.remove(&peer_id) {
            for webrtc_pad in consumer.webrtc_pads.clone().values() {
                // let transceiver = webrtc_pad
                //     .pad
                //     .property::<gst_webrtc::WebRTCRTPTransceiver>("transceiver");

                // if let Some(mid) = transceiver.mid() {
                //     state
                //         .mids
                //         .insert(mid.to_string(), webrtc_pad.stream_name.clone());
                // }

                if let Some(stream) = state
                    .streams
                    .get(&webrtc_pad.stream_name) {
                        if let Err(err) =
                        consumer.connect_input_stream(element, webrtc_pad, stream, &state.pipeline) {
                            gst::error!(
                                CAT,
                                obj: element,
                                "Failed to connect input stream {} for consumer {}: {}",
                                webrtc_pad.stream_name,
                                peer_id,
                                err
                            );
                            remove = true;
                            break;
                        }
                } else {
                    gst::error!(
                        CAT,
                        obj: element,
                        "No producer to connect consumer {} to",
                        peer_id,
                    );
                    remove = true;
                    break;
                }
            }

            state.pipeline.debug_to_dot_file_with_ts(
                gst::DebugGraphDetails::all(),
                format!("webrtcsink-consumer-peerId-{}-remote-description-set", peer_id,),
            );

            if remove {
                state.finalize_consumer(element, &mut consumer, true);
            } else {
                state.consumers.insert(consumer.peer_id.clone(), consumer);
            }
        }
    }

    /// Called by the signaller with an ice candidate
    pub fn handle_ice(
        &self,
        _element: &super::WebRTCSink,
        peer_id: &str,
        sdp_m_line_index: Option<u32>,
        _sdp_mid: Option<String>,
        candidate: &str,
    ) -> Result<(), WebRTCSinkError> {
        let state = self.state.lock().unwrap();

        let sdp_m_line_index = sdp_m_line_index.ok_or(WebRTCSinkError::MandatorySdpMlineIndex)?;

        if let Some(consumer) = state.consumers.get(peer_id) {
            gst::trace!(CAT, "adding ice candidate for peer {}", peer_id);
            consumer
                .webrtcbin
                .emit_by_name::<()>("add-ice-candidate", &[&sdp_m_line_index, &candidate]);
            Ok(())
        } else {
            Err(WebRTCSinkError::NoConsumerWithId(peer_id.to_string()))
        }
    }

    /// Called by the signaller with an answer to our offer
    pub fn handle_sdp(
        &self,
        element: &super::WebRTCSink,
        peer_id: &str,
        desc: &gst_webrtc::WebRTCSessionDescription,
    ) -> Result<(), WebRTCSinkError> {
        let mut state = self.state.lock().unwrap();

        if let Some(consumer) = state.consumers.get_mut(peer_id) {
            let sdp = desc.sdp();

            consumer.sdp = Some(sdp.to_owned());

            for webrtc_pad in consumer.webrtc_pads.values_mut() {
                let media_idx = webrtc_pad.media_idx;
                /* TODO: support partial answer, webrtcbin doesn't seem
                 * very well equipped to deal with this at the moment */
                if let Some(media) = sdp.media(media_idx) {
                    if media.attribute_val("inactive").is_some() {
                        let media_str = sdp
                            .media(webrtc_pad.media_idx)
                            .and_then(|media| media.as_text().ok());

                        gst::warning!(
                            CAT,
                            "consumer {} refused media {}: {:?}",
                            peer_id,
                            media_idx,
                            media_str
                        );
                        state.remove_consumer(element, peer_id, true);

                        return Err(WebRTCSinkError::ConsumerRefusedMedia {
                            peer_id: peer_id.to_string(),
                            media_idx,
                        });
                    }
                }
            }

            consumer
                .webrtcbin
                .emit_by_name::<()>("set-remote-description", &[desc, &None::<gst::Promise>]);

            Ok(())
        } else {
            Err(WebRTCSinkError::NoConsumerWithId(peer_id.to_string()))
        }
    }

    fn sink_event(&self, pad: &gst::Pad, element: &super::WebRTCSink, event: gst::Event) -> bool {
        use gst::EventView;

        match event.view() {
            EventView::Caps(e) => {
                if let Some(caps) = pad.current_caps() {
                    if caps.is_strictly_equal(e.caps()) {
                        // Nothing changed
                        true
                    } else {
                        gst::error!(CAT, obj: pad, "Renegotiation is not supported");
                        false
                    }
                } else {
                    gst::info!(CAT, obj: pad, "Received caps event {:?}", e);

                    let mut all_pads_have_caps = true;

                    let mut state = self.state.lock().unwrap();

                    state
                        .streams
                        .iter_mut()
                        .for_each(|(_, mut stream)| {
                            if stream.sink_pad.upcast_ref::<gst::Pad>() == pad {
                                stream.in_caps = Some(e.caps().to_owned());
                            } else if stream.in_caps.is_none() {
                                all_pads_have_caps = false;
                            }
                        });

                    if all_pads_have_caps {
                        state.codecs = self.lookup_codecs(&element);
                        state.pipeline_prepared = true;
                        self.prepare_pipeline(&element, &mut state);
                        state.maybe_start_signaller(&element);
                    }

                    pad.event_default(Some(element), event)
                }
            }
            _ => pad.event_default(Some(element), event),
        }
    }
}

#[glib::object_subclass]
impl ObjectSubclass for WebRTCSink {
    const NAME: &'static str = "RsWebRTCSink";
    type Type = super::WebRTCSink;
    type ParentType = gst::Bin;
    type Interfaces = (gst::ChildProxy, gst_video::Navigation);
}

impl ObjectImpl for WebRTCSink {
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPERTIES: Lazy<Vec<glib::ParamSpec>> = Lazy::new(|| {
            vec![
                glib::ParamSpecBoxed::new(
                    "video-caps",
                    "Video encoder caps",
                    "Governs what video codecs will be proposed",
                    gst::Caps::static_type(),
                    glib::ParamFlags::READWRITE | gst::PARAM_FLAG_MUTABLE_READY,
                ),
                glib::ParamSpecBoxed::new(
                    "audio-caps",
                    "Audio encoder caps",
                    "Governs what audio codecs will be proposed",
                    gst::Caps::static_type(),
                    glib::ParamFlags::READWRITE | gst::PARAM_FLAG_MUTABLE_READY,
                ),
                glib::ParamSpecString::new(
                    "stun-server",
                    "STUN Server",
                    "The STUN server of the form stun://hostname:port",
                    DEFAULT_STUN_SERVER,
                    glib::ParamFlags::READWRITE,
                ),
                glib::ParamSpecString::new(
                    "turn-server",
                    "TURN Server",
                    "The TURN server of the form turn(s)://username:password@host:port.",
                    None,
                    glib::ParamFlags::READWRITE,
                ),
                glib::ParamSpecUInt::new(
                    "bitrate",
                    "Bitrate",
                    "Bitrate to use (in bit/sec)",
                    1,
                    u32::MAX as u32,
                    DEFAULT_BITRATE,
                    glib::ParamFlags::READWRITE | gst::PARAM_FLAG_MUTABLE_READY,
                ),
                glib::ParamSpecBoolean::new(
                    "enable-data-channel-navigation",
                    "Enable data channel navigation",
                    "Enable navigation events through a dedicated WebRTCDataChannel",
                    DEFAULT_ENABLE_DATA_CHANNEL_NAVIGATION,
                    glib::ParamFlags::READWRITE | gst::PARAM_FLAG_MUTABLE_READY
                ),
                glib::ParamSpecString::new(
                    "display-name",
                    "Display name",
                    "The display name of the producer",
                    DEFAULT_DISPLAY_NAME,
                    glib::ParamFlags::READWRITE,
                ),
            ]
        });

        PROPERTIES.as_ref()
    }

    fn set_property(
        &self,
        _obj: &Self::Type,
        _id: usize,
        value: &glib::Value,
        pspec: &glib::ParamSpec,
    ) {
        match pspec.name() {
            "video-caps" => {
                let mut settings = self.settings.lock().unwrap();
                settings.video_caps = value
                    .get::<Option<gst::Caps>>()
                    .expect("type checked upstream")
                    .unwrap_or_else(gst::Caps::new_empty);
            }
            "audio-caps" => {
                let mut settings = self.settings.lock().unwrap();
                settings.audio_caps = value
                    .get::<Option<gst::Caps>>()
                    .expect("type checked upstream")
                    .unwrap_or_else(gst::Caps::new_empty);
            }
            "stun-server" => {
                let mut settings = self.settings.lock().unwrap();
                settings.stun_server = value
                    .get::<Option<String>>()
                    .expect("type checked upstream")
            }
            "turn-server" => {
                let mut settings = self.settings.lock().unwrap();
                settings.turn_server = value
                    .get::<Option<String>>()
                    .expect("type checked upstream")
            }
            "bitrate" => {
                let mut settings = self.settings.lock().unwrap();
                settings.bitrate = value.get::<u32>().expect("type checked upstream");
            }
            "enable-data-channel-navigation" => {
                let mut settings = self.settings.lock().unwrap();
                settings.enable_data_channel_navigation =
                    value.get::<bool>().expect("type checked upstream");
            }
            "display-name" => {
                let mut settings = self.settings.lock().unwrap();
                settings.display_name = value
                    .get::<Option<String>>()
                    .expect("type checked upstream")
            }
            _ => unimplemented!(),
        }
    }

    fn property(&self, _obj: &Self::Type, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        match pspec.name() {
            "video-caps" => {
                let settings = self.settings.lock().unwrap();
                settings.video_caps.to_value()
            }
            "audio-caps" => {
                let settings = self.settings.lock().unwrap();
                settings.audio_caps.to_value()
            }
            "stun-server" => {
                let settings = self.settings.lock().unwrap();
                settings.stun_server.to_value()
            }
            "turn-server" => {
                let settings = self.settings.lock().unwrap();
                settings.turn_server.to_value()
            }
            "bitrate" => {
                let settings = self.settings.lock().unwrap();
                settings.bitrate.to_value()
            }
            "enable-data-channel-navigation" => {
                let settings = self.settings.lock().unwrap();
                settings.enable_data_channel_navigation.to_value()
            }
            "display-name" => {
                let settings = self.settings.lock().unwrap();
                settings.display_name.to_value()
            }
            _ => unimplemented!(),
        }
    }

    fn signals() -> &'static [glib::subclass::Signal] {
        static SIGNALS: Lazy<Vec<glib::subclass::Signal>> = Lazy::new(|| {
            vec![
                /*
                 * RsWebRTCSink::consumer-added:
                 * @consumer_id: Identifier of the consumer added
                 * @webrtcbin: The new webrtcbin
                 *
                 * This signal can be used to tweak @webrtcbin, creating a data
                 * channel for example.
                 */
                glib::subclass::Signal::builder(
                    "consumer-added",
                    &[
                        String::static_type().into(),
                        gst::Element::static_type().into(),
                    ],
                    glib::types::Type::UNIT.into(),
                )
                .build(),
                /*
                 * RsWebRTCSink::consumer_removed:
                 * @consumer_id: Identifier of the consumer that was removed
                 * @webrtcbin: The webrtcbin connected to the newly removed consumer
                 *
                 * This signal is emitted right after the connection with a consumer
                 * has been dropped.
                 */
                glib::subclass::Signal::builder(
                    "consumer-removed",
                    &[
                        String::static_type().into(),
                        gst::Element::static_type().into(),
                    ],
                    glib::types::Type::UNIT.into(),
                )
                .build(),
                /*
                 * RsWebRTCSink::get_consumers:
                 *
                 * List all consumers (by ID).
                 */
                glib::subclass::Signal::builder(
                    "get-consumers",
                    &[],
                    <Vec<String>>::static_type().into(),
                )
                .action()
                .class_handler(|_, args| {
                    let element = args[0].get::<super::WebRTCSink>().expect("signal arg");
                    let this = element.imp();

                    let res = Some(
                        this.state
                            .lock()
                            .unwrap()
                            .consumers
                            .keys()
                            .cloned()
                            .collect::<Vec<String>>()
                            .to_value(),
                    );
                    res
                })
                .build(),
                /*
                 * RsWebRTCSink::encoder-setup:
                 * @consumer_id: Identifier of the consumer
                 * @pad_name: The name of the corresponding input pad
                 * @encoder: The constructed encoder
                 *
                 * This signal can be used to tweak @encoder properties.
                 *
                 * Returns: True if the encoder is entirely configured,
                 * False to let other handlers run
                 */
                glib::subclass::Signal::builder(
                    "encoder-setup",
                    &[
                        String::static_type().into(),
                        gst::Element::static_type().into(),
                    ],
                    bool::static_type().into(),
                )
                .accumulator(|_hint, _ret, value| !value.get::<bool>().unwrap())
                .class_handler(|_, args| {
                    let element = args[0].get::<super::WebRTCSink>().expect("signal arg");
                    let enc = args[2].get::<gst::Element>().unwrap();

                    gst::debug!(
                        CAT,
                        obj: &element,
                        "applying default configuration on encoder {:?}",
                        enc
                    );

                    let this = element.imp();
                    let settings = this.settings.lock().unwrap();
                    configure_encoder(&enc, settings.bitrate);

                    // Return false here so that latter handlers get called
                    Some(false.to_value())
                })
                .build(),
            ]
        });

        SIGNALS.as_ref()
    }

    fn constructed(&self, obj: &Self::Type) {
        self.parent_constructed(obj);

        obj.set_suppressed_flags(gst::ElementFlags::SINK | gst::ElementFlags::SOURCE);
        obj.set_element_flags(gst::ElementFlags::SINK);
    }
}

impl GstObjectImpl for WebRTCSink {}

impl ElementImpl for WebRTCSink {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static ELEMENT_METADATA: Lazy<gst::subclass::ElementMetadata> = Lazy::new(|| {
            gst::subclass::ElementMetadata::new(
                "WebRTCSink",
                "Sink/Network/WebRTC",
                "WebRTC sink",
                "Mathieu Duponchelle <mathieu@centricular.com>",
            )
        });

        Some(&*ELEMENT_METADATA)
    }

    fn pad_templates() -> &'static [gst::PadTemplate] {
        static PAD_TEMPLATES: Lazy<Vec<gst::PadTemplate>> = Lazy::new(|| {
            let caps = gst::Caps::builder_full()
                .structure(gst::Structure::builder("video/x-raw").build())
                .structure_with_features(
                    gst::Structure::builder("video/x-raw").build(),
                    gst::CapsFeatures::new(&[CUDA_MEMORY_FEATURE]),
                )
                .structure_with_features(
                    gst::Structure::builder("video/x-raw").build(),
                    gst::CapsFeatures::new(&[GL_MEMORY_FEATURE]),
                )
                .structure_with_features(
                    gst::Structure::builder("video/x-raw").build(),
                    gst::CapsFeatures::new(&[NVMM_MEMORY_FEATURE]),
                )
                .build();
            let video_pad_template = gst::PadTemplate::new(
                "video_%u",
                gst::PadDirection::Sink,
                gst::PadPresence::Request,
                &caps,
            )
            .unwrap();

            let caps = gst::Caps::builder("audio/x-raw").build();
            let audio_pad_template = gst::PadTemplate::new(
                "audio_%u",
                gst::PadDirection::Sink,
                gst::PadPresence::Request,
                &caps,
            )
            .unwrap();

            vec![video_pad_template, audio_pad_template]
        });

        PAD_TEMPLATES.as_ref()
    }

    fn request_new_pad(
        &self,
        element: &Self::Type,
        templ: &gst::PadTemplate,
        _name: Option<String>,
        _caps: Option<&gst::Caps>,
    ) -> Option<gst::Pad> {
        if element.current_state() > gst::State::Ready {
            gst::error!(CAT, "element pads can only be requested before starting");
            return None;
        }

        let mut state = self.state.lock().unwrap();

        let (name, payload) = if templ.name().starts_with("video_") {
            let name = format!("video_{}", state.video_serial);
            state.video_serial += 1;
            (name, 96)
        } else {
            let name = format!("audio_{}", state.audio_serial);
            state.audio_serial += 1;
            (name, 97)
        };

        let sink_pad = gst::GhostPad::builder_with_template(templ, Some(name.as_str()))
            .event_function(|pad, parent, event| {
                WebRTCSink::catch_panic_pad_function(
                    parent,
                    || false,
                    |sink, element| sink.sink_event(pad.upcast_ref(), element, event),
                )
            })
            .build();

        sink_pad.set_active(true).unwrap();
        sink_pad.use_fixed_caps();
        element.add_pad(&sink_pad).unwrap();

        let srrc = state.generate_ssrc();

        state.streams.insert(
            name,
            InputStream {
                sink_pad: sink_pad.clone(),
                producer: None,
                in_caps: None,
                clocksync: None,
                payload: Some(payload.clone()),
                tee: None,
                ssrc: srrc,
            },
        );

        Some(sink_pad.upcast())
    }

    fn change_state(
        &self,
        element: &Self::Type,
        transition: gst::StateChange,
    ) -> Result<gst::StateChangeSuccess, gst::StateChangeError> {
        if let gst::StateChange::ReadyToPaused = transition {

            if let Err(err) = self.prepare(element) {
                gst::element_error!(
                    element,
                    gst::StreamError::Failed,
                    ["Failed to prepare: {}", err]
                );
                return Err(gst::StateChangeError);
            }
        }

        let mut ret = self.parent_change_state(element, transition);

        match transition {
            gst::StateChange::PausedToReady => {

                if let Err(err) = self.unprepare(element) {
                    gst::element_error!(
                        element,
                        gst::StreamError::Failed,
                        ["Failed to unprepare: {}", err]
                    );
                    return Err(gst::StateChangeError);
                }
            }
            gst::StateChange::ReadyToPaused => {
                ret = Ok(gst::StateChangeSuccess::NoPreroll);
            }
            gst::StateChange::PausedToPlaying => {
                let mut state = self.state.lock().unwrap();
                state.maybe_start_signaller(element);
            }
            _ => (),
        }

        ret
    }
}

impl BinImpl for WebRTCSink {}

impl ChildProxyImpl for WebRTCSink {
    fn child_by_index(&self, _object: &Self::Type, _index: u32) -> Option<glib::Object> {
        None
    }

    fn children_count(&self, _object: &Self::Type) -> u32 {
        0
    }

    fn child_by_name(&self, _object: &Self::Type, name: &str) -> Option<glib::Object> {
        match name {
            "signaller" => Some(
                self.state
                    .lock()
                    .unwrap()
                    .signaller
                    .as_ref()
                    .as_ref()
                    .clone(),
            ),
            _ => None,
        }
    }
}

impl NavigationImpl for WebRTCSink {
    fn send_event(&self, _imp: &Self::Type, event_def: gst::Structure) {
        let mut state = self.state.lock().unwrap();
        let event = gst::event::Navigation::new(event_def);

        state.streams.iter_mut().for_each(|(_, stream)| {
            if stream.sink_pad.name().starts_with("video_") {
                gst::log!(CAT, "Navigating to: {:?}", event);
                // FIXME: Handle multi tracks.
                if !stream.sink_pad.push_event(event.clone()) {
                    gst::info!(CAT, "Could not send event: {:?}", event);
                }
            }
        });
    }
}
