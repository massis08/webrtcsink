use gst::ElementFactory;
use gst::prelude::*;
use super::webrtcsink::WebRTCSinkError;

#[derive(thiserror::Error, Debug)]
pub enum GstreamerError {
    #[error("Failed creating gstreamer element`{elem_name:?} when {called_from:?}`")]
    GstreamerCanNotCreateElement{
        called_from: String,
        elem_name: String,
    },
    #[error("Failed adding gstreamer elements when `{0}`")]
    GstreamerCanNotAddElement(String),
    #[error("Failed removing gstreamer elements when `{0}`")]
    GstreamerCanNotRemoveElement(String),
    #[error("Failed linking gstreamer elements when `{0}`")]
    GstreamerCanNotLinkElement(String),
    #[error("Failed linking gstreamer pads when `{0}`")]
    GstreamerCanNotLinkPad(String),
    #[error("Failed unlinking gstreamer pads when `{0}`")]
    GstreamerCanNotUnlinkPad(String),
    #[error("Failed creating pad template `{pad_template:?}` in gstreamer element {elem_name:?} when `{called_from:?}`")]
    GstreamerCanNotCreatePadTemplate {
        called_from: String,
        pad_template: String,
        elem_name: String,
    },
    #[error("Failed creating pad `{pad:?}` in gstreamer element {elem_name:?} when `{called_from:?}`")]
    GstreamerCanNotCreatePad {
        called_from: String,
        pad: String,
        elem_name: String,
    },
    #[error("Failed finding pad `{pad:?}` in gstreamer element {elem_name:?} when `{called_from:?}`")]
    GstreamerCanNotFindPad {
        called_from: String,
        pad: String,
        elem_name: String,
    },
    #[error("Failed adding ghost pad `{pad:?}` in gstreamer element {elem_name:?} when `{called_from:?}`")]
    GstreamerCanNotAddGhostPad {
        called_from: String,
        pad: String,
        elem_name: String,
    },
    #[error("Failed creating bin with description `{bin_description:?}` when `{called_from:?}`")]
    GstreamerCanNotCreateBin {
        called_from: String,
        bin_description: String,
    },
    #[error("Failed syncing state of `{elem_name:?}` with parent when `{called_from:?}`")]
    GstreamerCanNotSyncState {
        called_from: String,
        elem_name: String,
    },
    #[error("Failed setting state of `{elem_name:?}` with parent when `{called_from:?}`")]
    GstreamerCanNotSetState {
        called_from: String,
        elem_name: String,
    },
    #[error("Failed setting target in ghost pad `{pad:?} when `{called_from:?}`")]
    GstreamerCanNotSetTargetPad {
        called_from: String,
        pad: String,
    }
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

pub fn gstreamer_create_element(element: &str, name: Option<&str>, called_from: &str) -> Result<gst::Element, GstreamerError> {
    match gst::ElementFactory::make(element, name){
        Ok(elem) => Ok(elem),
        Err(_error) => Err(GstreamerError::GstreamerCanNotCreateElement { 
            called_from: called_from.to_string(), 
            elem_name: element.to_string(), 
        }),
    }
}

pub fn gstreamer_create_element_from_factory(element: &ElementFactory, name: Option<&str>, called_from: &str) -> Result<gst::Element, GstreamerError> {
    match element.create(name) {
        Ok(elem) => Ok(elem),
        Err(_error) => Err(GstreamerError::GstreamerCanNotCreateElement{
            called_from: called_from.to_string(), 
            elem_name: element.name().to_string(), 
        }),
    }
}

pub fn gstreamer_add_many(elements: &[&gst::Element], pipeline: &gst::Pipeline, called_from: &str) -> Result<(), GstreamerError> {
    match pipeline.add_many(elements) {
        Ok(_) => Ok(()),
        Err(_error) => Err(GstreamerError::GstreamerCanNotAddElement(called_from.to_string())),
    }
}

pub fn gstreamer_add(element: &gst::Element, pipeline: &gst::Pipeline, called_from: &str) -> Result<(), GstreamerError> {
    match pipeline.add(element) {
        Ok(_) => Ok(()),
        Err(_error) => Err(GstreamerError::GstreamerCanNotAddElement(called_from.to_string())),
    }
}

pub fn gstreamer_remove(element: &gst::Element, pipeline: &gst::Pipeline, called_from: &str) -> Result<(), GstreamerError> {
    match pipeline.remove(element) {
        Ok(_) => Ok(()),
        Err(_error) => Err(GstreamerError::GstreamerCanNotRemoveElement(called_from.to_string())),
    }
}

pub fn gstreamer_bin_add(element: &gst::Element, bin: &gst::Bin, called_from: &str) -> Result<(), GstreamerError> {
    match bin.add(element) {
        Ok(_) => Ok(()),
        Err(_error) => Err(GstreamerError::GstreamerCanNotAddElement(called_from.to_string())),
    }
}

pub fn gstreamer_bin_add_many(elements: &[&gst::Element], bin: &gst::Bin, called_from: &str) -> Result<(), GstreamerError> {
    match bin.add_many(elements) {
        Ok(_) => Ok(()),
        Err(_error) => Err(GstreamerError::GstreamerCanNotAddElement(called_from.to_string())),
    }
}

pub fn gstreamer_link_many(elements: &[&gst::Element], called_from: &str) -> Result<(), GstreamerError> {
    match gst::Element::link_many(elements) {
        Ok(elem) => Ok(elem),
        Err(_error) => Err(GstreamerError::GstreamerCanNotLinkElement(called_from.to_string())),
    }
}

pub fn gstreamer_link(src: &gst::Element, sink: &gst::Element, called_from: &str) -> Result<(), GstreamerError> {
    match src.link(sink) {
        Ok(()) => Ok(()),
        Err(_error) => Err(GstreamerError::GstreamerCanNotLinkElement(called_from.to_string())),
    }
}

pub fn gstreamer_link_pads(src: &gst::Pad, sink: &gst::Pad, called_from: &str) -> Result<gst::PadLinkSuccess, GstreamerError> {
    match src.link(sink) {
        Ok(pad) => Ok(pad),
        Err(_error) => Err(GstreamerError::GstreamerCanNotLinkPad(called_from.to_string())),
    }
}

pub fn gstreamer_unlink_pads(src: &gst::Pad, sink: &gst::Pad, called_from: &str) -> Result<(), GstreamerError> {
    match src.unlink(sink) {
        Ok(_) => Ok(()),
        Err(_error) => Err(GstreamerError::GstreamerCanNotUnlinkPad(called_from.to_string())),
    }
}


pub fn gstreamer_get_pad_template(elem: &gst::Element, pad_name: &str, called_from: &str) -> Result<gst::PadTemplate, GstreamerError> {
    match elem.pad_template(pad_name) {
        Some(value) => Ok(value),
        None => Err(GstreamerError::GstreamerCanNotCreatePadTemplate{
                        called_from: called_from.to_string(),
                        pad_template: pad_name.to_string(),
                        elem_name: elem.name().to_string(),
        })  
    }
}

pub fn gstreamer_request_pad_with_pad_template(elem: &gst::Element, pad_name: &str, called_from: &str) -> Result<gst::Pad, GstreamerError> {

    let pad_template = gstreamer_get_pad_template(elem, pad_name, called_from)?;

    match elem.request_pad(&pad_template, None, None) {
        Some(value) => Ok(value),
        None => Err(GstreamerError::GstreamerCanNotCreatePad {
            called_from: called_from.to_string(),
            pad: pad_name.to_string(),
            elem_name: elem.name().to_string(),
        })  
    }
}

pub fn gstreamer_request_simple_pad(elem: &gst::Element, pad_name: &str, called_from: &str) -> Result<gst::Pad, GstreamerError> {
    match elem.request_pad_simple(pad_name) {
        Some(value) => Ok(value),
        None => Err(GstreamerError::GstreamerCanNotCreatePad {
            called_from: called_from.to_string(),
            pad: pad_name.to_string(),
            elem_name: elem.name().to_string(),
        })  
    }
}

pub fn gstreamer_get_static_pad(elem: &gst::Element, pad_name: &str, called_from: &str) -> Result<gst::Pad, GstreamerError> {
    match elem.static_pad(pad_name) {
        Some(value) => Ok(value),
        None => Err(GstreamerError::GstreamerCanNotFindPad {
            called_from: called_from.to_string(),
            pad: pad_name.to_string(),
            elem_name: elem.name().to_string(),
        })  
    }
}

pub fn gstreamer_get_ghost_pad(elem: &gst::Element, pad_name: &str, called_from: &str) -> Result<gst::GhostPad, GstreamerError> {
    let target_pad = gstreamer_get_static_pad(elem, pad_name, called_from)?;
    
    match gst::GhostPad::with_target(Some(pad_name), &target_pad) {
        Ok(ghost_pad) => Ok(ghost_pad),
        Err(_) => Err(GstreamerError::GstreamerCanNotCreatePad {
            called_from: called_from.to_string(),
            pad: pad_name.to_string(),
            elem_name: elem.name().to_string(),
        })  
    }
}

pub fn gstreamer_add_ghost_pad(elem: &gst::Element, pad: &gst::GhostPad, called_from: &str) -> Result<(), GstreamerError> {
    match elem.add_pad(pad) {
        Ok(_) => Ok(()),
        Err(_) => Err(GstreamerError::GstreamerCanNotAddGhostPad {
            called_from: called_from.to_string(),
            pad: pad.name().to_string(),
            elem_name: elem.name().to_string(),
        })
    }
}

pub fn gstreamer_ghost_pad_set_target(elem: &gst::Element, pad: &gst::GhostPad, pad_name: &str, called_from: &str) -> Result<(), GstreamerError> {
    let target_pad = gstreamer_get_static_pad(elem, pad_name, called_from)?;
    
    match pad.set_target(Some(&target_pad)) {
        Ok(_) => Ok(()),
        Err(_) => Err(GstreamerError::GstreamerCanNotSetTargetPad {
            called_from: called_from.to_string(),
            pad: pad_name.to_string(),
        })  
    }
}

pub fn gstreamer_create_bin_from_description(bin_description: &str, ghost_unlinked_pad: bool, called_from: &str) -> Result<gst::Bin, GstreamerError> {
    match gst::parse_bin_from_description(bin_description, ghost_unlinked_pad) {
        Ok(bin) => Ok(bin),
        Err(_) => Err(GstreamerError::GstreamerCanNotCreateBin {
            called_from: called_from.to_string(),
            bin_description: bin_description.to_string(),
        })
    }
}

pub fn gstreamer_sycn_state_with_parent(elem: &gst::Element, called_from: &str) -> Result<(), GstreamerError> {
    match elem.sync_state_with_parent() {
        Ok(()) => Ok(()),
        Err(_) => Err(GstreamerError::GstreamerCanNotSyncState {
            called_from: called_from.to_string(),
            elem_name: elem.name().to_string(),
        })
    }
}

pub fn gstreamer_pipeline_set_state(pipeline: &gst::Pipeline, state: gst::State, called_from: &str) -> Result<(), GstreamerError> {
    match pipeline.set_state(state) {
        Ok(_) => Ok(()),
        Err(_) => Err(GstreamerError::GstreamerCanNotSetState {
            called_from: called_from.to_string(),
            elem_name: "pipeline".to_string(),
        })
    }
}


pub fn gstreamer_element_set_state(elem: &gst::Element, state: gst::State, called_from: &str) -> Result<(), GstreamerError> {
    match elem.set_state(state) {
        Ok(_) => Ok(()),
        Err(_) => Err(GstreamerError::GstreamerCanNotSetState {
            called_from: called_from.to_string(),
            elem_name: elem.name().to_string(),
        })
    }
}