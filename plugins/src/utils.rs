use gst::ElementFactory;
use gst::glib::BoolError;
use gst::prelude::*;
use super::webrtcsink::WebRTCSinkError;

#[derive(thiserror::Error, Debug)]
pub enum GstreamerError {
    #[error("failed to create gstreamer element `{0}`")]
    GstreamerCanNotCreateElement(String),
    #[error("failed to add gstreamer elements")]
    GstreamerCanNotAddElement,
    #[error("failed to remove gstreamer elements")]
    GstreamerCanNotRemoveElement,
    #[error("failed to link gstreamer elements")]
    GstreamerCanNotLinkElement,
    #[error("failed to link gstreamer pads")]
    GstreamerCanNotLinkPad,
    #[error("failed to unlink gstreamer pads")]
    GstreamerCanNotUnlinkPad,
    #[error("failed to create pad template `{pad_template:?}` in gstreamer element `{elem_name:?}`")]
    GstreamerCanNotCreatePadTemplate {
        pad_template: String,
        elem_name: String,
    },
    #[error("failed to create pad `{pad:?}` in gstreamer element `{elem_name:?}`")]
    GstreamerCanNotCreatePad {
        pad: String,
        elem_name: String,
    },
    #[error("failed to find pad `{pad:?}` in gstreamer element `{elem_name:?}`")]
    GstreamerCanNotFindPad {
        pad: String,
        elem_name: String,
    },
    #[error("failed to add ghost pad `{pad:?}` in gstreamer element `{elem_name:?}`")]
    GstreamerCanNotAddGhostPad {
        pad: String,
        elem_name: String,
    },
    #[error("failed to create bin with description `{0:?}`")]
    GstreamerCanNotCreateBin(String),
    #[error("failed to sync state of `{0:?}` with parent")]
    GstreamerCanNotSyncState(String),
    #[error("failed to set state of `{0:?}`")]
    GstreamerCanNotSetState(String),
    #[error("failed to set target in ghost pad `{0:?}")]
    GstreamerCanNotSetTargetPad(String)
}

pub fn webrtcsink_consumer_error_or_ok<T>(result: Result<T, GstreamerError>, peer_id: &str) -> Result<T,WebRTCSinkError> {
        result.map_err(|err| {
            WebRTCSinkError::ConsumerPipelineError {
                peer_id: peer_id.to_string(),
                details: err.to_string(),
            }
        })
}

pub fn webrtcsink_consumer_some_or_none<T>(result: Option<T>, peer_id: &str, error_message: &str) -> Result<T,WebRTCSinkError> {
    match result {
        Some(t) => Ok(t),
        None => Err(WebRTCSinkError::ConsumerPipelineError { 
            peer_id: peer_id.to_string(), 
            details: error_message.to_string()
        })
    }
}

pub fn webrtcsink_producer_error_or_ok<T>(result: Result<T, GstreamerError>) -> Result<T,WebRTCSinkError> {
    result.map_err(|err| {
        WebRTCSinkError::ProducerPipelineError {
            details: err.to_string(),
        }
    })
}

pub fn webrtcsink_producer_some_or_none<T>(result: Option<T>, error_message: &str) -> Result<T,WebRTCSinkError> {
    match result {
        Some(t) => Ok(t),
        None => Err(WebRTCSinkError::ProducerPipelineError { 
            details: error_message.to_string()
        })
    }
}

pub fn webrtcsink_prepare_error_or_ok<T>(result: Result<T, GstreamerError>) -> Result<T,WebRTCSinkError> {
    result.map_err(|err| {
        WebRTCSinkError::PrepareWebrtcsinkError {
            details: err.to_string(),
        }
    })
}

pub fn _webrtcsink_prepare_some_or_none<T>(result: Option<T>, error_message: &str) -> Result<T,WebRTCSinkError> {
    match result {
        Some(t) => Ok(t),
        None => Err(WebRTCSinkError::PrepareWebrtcsinkError { 
            details: error_message.to_string()
        })
    }
}

pub fn webrtcsink_unprepare_error_or_ok<T>(result: Result<T, GstreamerError>) -> Result<T,WebRTCSinkError> {
    result.map_err(|err| {
        WebRTCSinkError::UnprepareWebrtcsinkError {
            details: err.to_string(),
        }
    })
}

pub fn webrtcsink_unprepare_some_or_none<T>(result: Option<T>, error_message: &str) -> Result<T,WebRTCSinkError> {
    match result {
        Some(t) => Ok(t),
        None => Err(WebRTCSinkError::UnprepareWebrtcsinkError { 
            details: error_message.to_string()
        })
    }
}

pub fn signaller_some_or_none<T>(result: Option<T>, error_message: &str) -> Result<T,WebRTCSinkError> {
    match result {
        Some(t) => Ok(t),
        None => Err(WebRTCSinkError::FailedSignaller(error_message.to_string())),
    }
}

pub fn signaller_error_or_ok<T>(result: Result<T, BoolError>, error_message: &str) -> Result<T,WebRTCSinkError> {
    result.map_err(|_err| {
        WebRTCSinkError::FailedSignaller(error_message.to_string())
    })
}

pub fn gstreamer_create_element(element: &str, name: Option<&str>) -> Result<gst::Element, GstreamerError> {
    match gst::ElementFactory::make(element, name){
        Ok(elem) => Ok(elem),
        Err(_error) => Err(GstreamerError::GstreamerCanNotCreateElement(element.to_string())),
    }
}

pub fn gstreamer_create_element_from_factory(element: &ElementFactory, name: Option<&str>) -> Result<gst::Element, GstreamerError> {
    match element.create(name) {
        Ok(elem) => Ok(elem),
        Err(_error) => Err(GstreamerError::GstreamerCanNotCreateElement(element.name().to_string())),
    }
}

pub fn gstreamer_add_many(elements: &[&gst::Element], pipeline: &gst::Pipeline) -> Result<(), GstreamerError> {
    match pipeline.add_many(elements) {
        Ok(_) => Ok(()),
        Err(_error) => Err(GstreamerError::GstreamerCanNotAddElement),
    }
}

pub fn gstreamer_add(element: &gst::Element, pipeline: &gst::Pipeline) -> Result<(), GstreamerError> {
    match pipeline.add(element) {
        Ok(_) => Ok(()),
        Err(_error) => Err(GstreamerError::GstreamerCanNotAddElement),
    }
}

pub fn gstreamer_remove(element: &gst::Element, pipeline: &gst::Pipeline) -> Result<(), GstreamerError> {
    match pipeline.remove(element) {
        Ok(_) => Ok(()),
        Err(_error) => Err(GstreamerError::GstreamerCanNotRemoveElement),
    }
}

pub fn gstreamer_bin_add(element: &gst::Element, bin: &gst::Bin) -> Result<(), GstreamerError> {
    match bin.add(element) {
        Ok(_) => Ok(()),
        Err(_error) => Err(GstreamerError::GstreamerCanNotAddElement),
    }
}

pub fn gstreamer_bin_add_many(elements: &[&gst::Element], bin: &gst::Bin) -> Result<(), GstreamerError> {
    match bin.add_many(elements) {
        Ok(_) => Ok(()),
        Err(_error) => Err(GstreamerError::GstreamerCanNotAddElement),
    }
}

pub fn gstreamer_link_many(elements: &[&gst::Element]) -> Result<(), GstreamerError> {
    match gst::Element::link_many(elements) {
        Ok(elem) => Ok(elem),
        Err(_error) => Err(GstreamerError::GstreamerCanNotLinkElement),
    }
}

pub fn gstreamer_link(src: &gst::Element, sink: &gst::Element) -> Result<(), GstreamerError> {
    match src.link(sink) {
        Ok(()) => Ok(()),
        Err(_error) => Err(GstreamerError::GstreamerCanNotLinkElement),
    }
}

pub fn gstreamer_link_pads(src: &gst::Pad, sink: &gst::Pad) -> Result<gst::PadLinkSuccess, GstreamerError> {
    match src.link(sink) {
        Ok(pad) => Ok(pad),
        Err(_error) => Err(GstreamerError::GstreamerCanNotLinkPad),
    }
}

pub fn gstreamer_unlink_pads(src: &gst::Pad, sink: &gst::Pad) -> Result<(), GstreamerError> {
    match src.unlink(sink) {
        Ok(_) => Ok(()),
        Err(_error) => Err(GstreamerError::GstreamerCanNotUnlinkPad),
    }
}


pub fn gstreamer_get_pad_template(elem: &gst::Element, pad_name: &str) -> Result<gst::PadTemplate, GstreamerError> {
    match elem.pad_template(pad_name) {
        Some(value) => Ok(value),
        None => Err(GstreamerError::GstreamerCanNotCreatePadTemplate{
                        pad_template: pad_name.to_string(),
                        elem_name: elem.name().to_string(),
        })  
    }
}

pub fn gstreamer_request_pad_with_pad_template(elem: &gst::Element, pad_name: &str) -> Result<gst::Pad, GstreamerError> {

    let pad_template = gstreamer_get_pad_template(elem, pad_name)?;

    match elem.request_pad(&pad_template, None, None) {
        Some(value) => Ok(value),
        None => Err(GstreamerError::GstreamerCanNotCreatePad {
            pad: pad_name.to_string(),
            elem_name: elem.name().to_string(),
        })  
    }
}

pub fn gstreamer_request_simple_pad(elem: &gst::Element, pad_name: &str) -> Result<gst::Pad, GstreamerError> {
    match elem.request_pad_simple(pad_name) {
        Some(value) => Ok(value),
        None => Err(GstreamerError::GstreamerCanNotCreatePad {
            pad: pad_name.to_string(),
            elem_name: elem.name().to_string(),
        })  
    }
}

pub fn gstreamer_get_static_pad(elem: &gst::Element, pad_name: &str) -> Result<gst::Pad, GstreamerError> {
    match elem.static_pad(pad_name) {
        Some(value) => Ok(value),
        None => Err(GstreamerError::GstreamerCanNotFindPad {
            pad: pad_name.to_string(),
            elem_name: elem.name().to_string(),
        })  
    }
}

pub fn gstreamer_get_ghost_pad(elem: &gst::Element, pad_name: &str) -> Result<gst::GhostPad, GstreamerError> {
    let target_pad = gstreamer_get_static_pad(elem, pad_name)?;
    
    match gst::GhostPad::with_target(Some(pad_name), &target_pad) {
        Ok(ghost_pad) => Ok(ghost_pad),
        Err(_) => Err(GstreamerError::GstreamerCanNotCreatePad {
            pad: pad_name.to_string(),
            elem_name: elem.name().to_string(),
        })  
    }
}

pub fn gstreamer_add_ghost_pad(elem: &gst::Element, pad: &gst::GhostPad) -> Result<(), GstreamerError> {
    match elem.add_pad(pad) {
        Ok(_) => Ok(()),
        Err(_) => Err(GstreamerError::GstreamerCanNotAddGhostPad {
            pad: pad.name().to_string(),
            elem_name: elem.name().to_string(),
        })
    }
}

pub fn gstreamer_ghost_pad_set_target(elem: &gst::Element, pad: &gst::GhostPad, pad_name: &str) -> Result<(), GstreamerError> {
    let target_pad = gstreamer_get_static_pad(elem, pad_name)?;
    
    match pad.set_target(Some(&target_pad)) {
        Ok(_) => Ok(()),
        Err(_) => Err(GstreamerError::GstreamerCanNotSetTargetPad(pad_name.to_string()))  
    }
}

pub fn gstreamer_create_bin_from_description(bin_description: &str, ghost_unlinked_pad: bool) -> Result<gst::Bin, GstreamerError> {
    match gst::parse_bin_from_description(bin_description, ghost_unlinked_pad) {
        Ok(bin) => Ok(bin),
        Err(_) => Err(GstreamerError::GstreamerCanNotCreateBin(bin_description.to_string())),
    }
}

pub fn gstreamer_sycn_state_with_parent(elem: &gst::Element) -> Result<(), GstreamerError> {
    match elem.sync_state_with_parent() {
        Ok(()) => Ok(()),
        Err(_) => Err(GstreamerError::GstreamerCanNotSyncState(elem.name().to_string())),
    }
}

pub fn gstreamer_pipeline_set_state(pipeline: &gst::Pipeline, state: gst::State) -> Result<(), GstreamerError> {
    match pipeline.set_state(state) {
        Ok(_) => Ok(()),
        Err(_) => Err(GstreamerError::GstreamerCanNotSetState("pipeline".to_string())),
    }
}


pub fn gstreamer_element_set_state(elem: &gst::Element, state: gst::State) -> Result<(), GstreamerError> {
    match elem.set_state(state) {
        Ok(_) => Ok(()),
        Err(_) => Err(GstreamerError::GstreamerCanNotSetState(elem.name().to_string())),
    }
}