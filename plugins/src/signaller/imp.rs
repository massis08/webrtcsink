use crate::webrtcsink::{WebRTCSink, WebRTCSinkError};
use anyhow::{anyhow, Error};
use async_std::task;
use async_tungstenite::tungstenite::Message as WsMessage;
use futures::channel::mpsc;
use futures::prelude::*;
use gst::glib;
use gst::glib::prelude::*;
use gst::subclass::prelude::*;
use once_cell::sync::Lazy;
use std::path::PathBuf;
use std::sync::Mutex;
use webrtcsink_protocol as p;
use crate::utils::*;

static CAT: Lazy<gst::DebugCategory> = Lazy::new(|| {
    gst::DebugCategory::new(
        "webrtcsink-signaller",
        gst::DebugColorFlags::empty(),
        Some("WebRTC sink signaller"),
    )
});

#[derive(Default)]
struct State {
    /// Sender for the websocket messages
    websocket_sender: Option<mpsc::Sender<p::IncomingMessage>>,
    send_task_handle: Option<task::JoinHandle<Result<(), Error>>>,
    receive_task_handle: Option<task::JoinHandle<()>>,
}

#[derive(Clone)]
struct Settings {
    address: Option<String>,
    cafile: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            address: Some("ws://127.0.0.1:8443".to_string()),
            cafile: None,
        }
    }
}

#[derive(Default)]
pub struct Signaller {
    state: Mutex<State>,
    settings: Mutex<Settings>,
}

impl Signaller {
    async fn connect(&self, element: &WebRTCSink) -> Result<(), Error> {

        let settings = match self.settings.lock() {
            Ok(settings) => Ok(settings.clone()),
            Err(_) => Err(WebRTCSinkError::FailedLockMutex("failed to get settings from mutex".to_string())),
        }?;

        let connector = if let Some(path) = settings.cafile {
            let cert = async_std::fs::read_to_string(&path).await?;
            let cert = async_native_tls::Certificate::from_pem(cert.as_bytes())?;
            let connector = async_native_tls::TlsConnector::new();
            Some(connector.add_root_certificate(cert))
        } else {
            None
        };

        let address = signaller_some_or_none(settings.address, "Failed to get address from settings")?;
        let (ws, _) = async_tungstenite::async_std::connect_async_with_tls_connector(
            address,
            connector,
        )
        .await?;

        gst::info!(CAT, obj: element, "connected");

        // Channel for asynchronously sending out websocket message
        let (mut ws_sink, mut ws_stream) = ws.split();

        // 1000 is completely arbitrary, we simply don't want infinite piling
        // up of messages as with unbounded
        let (mut websocket_sender, mut websocket_receiver) =
            mpsc::channel::<p::IncomingMessage>(1000);
        let element_clone = element.downgrade();
        let send_task_handle = task::spawn(async move {
            // while let Some(msg) = websocket_receiver.next().await {
            //     if let Some(element) = element_clone.upgrade() {
            //         gst::trace!(CAT, obj: &element, "Sending websocket message {:?}", msg);
            //     }
            //     ws_sink
            //         .send(WsMessage::Text(serde_json::to_string(&msg).unwrap()))
            //         .await?;
            // }

            loop {
                match async_std::future::timeout(
                    std::time::Duration::from_secs(30),
                    websocket_receiver.next(),
                )
                .await
                {
                    Ok(Some(msg)) => {
                        if let Some(element) = element_clone.upgrade() {
                            gst::trace!(CAT, obj: &element, "Sending websocket message {:?}", msg);
                            match serde_json::to_string(&msg) {
                                Ok(msg) => ws_sink.send(WsMessage::Text(msg)).await?,
                                Err(err) => element.handle_signalling_error(err.into()),
                            }
                        }                     
            
                    }
                    Ok(None) => {
                        break;
                    }
                    Err(_) => {
                        if let Some(element) = element_clone.upgrade() {
                            gst::trace!(CAT, obj: &element, "timeout, sending ping");
                        }
                        ws_sink.send(WsMessage::Ping(vec![])).await?;
                    }
                }
            }

            if let Some(element) = element_clone.upgrade() {
                gst::info!(CAT, obj: &element, "Done sending");
            }

            ws_sink.send(WsMessage::Close(None)).await?;
            ws_sink.close().await?;

            Ok::<(), Error>(())
        });

        websocket_sender
            .send(p::IncomingMessage::Register(p::RegisterMessage::Producer {
                display_name: element.property("display-name"),
            }))
            .await?;

        let element_clone = element.downgrade();
        let receive_task_handle = task::spawn(async move {
            while let Some(msg) = async_std::stream::StreamExt::next(&mut ws_stream).await {
                if let Some(element) = element_clone.upgrade() {
                    match msg {
                        Ok(WsMessage::Text(msg)) => {
                            gst::trace!(CAT, obj: &element, "Received message {}", msg);

                            if let Ok(msg) = serde_json::from_str::<p::OutgoingMessage>(&msg) {
                                match msg {
                                    p::OutgoingMessage::Registered(
                                        p::RegisteredMessage::Producer { peer_id, .. },
                                    ) => {
                                        gst::info!(
                                            CAT,
                                            obj: &element,
                                            "We are registered with the server, our peer id is {}",
                                            peer_id
                                        );
                                    }
                                    p::OutgoingMessage::Registered(_) => unreachable!(),
                                    p::OutgoingMessage::StartSession { peer_id } => {
                                        match element.create_webrtcbin_for_consumer(&peer_id) {
                                            Ok(webrtcbin) => {
                                                if let Err(err) = element.add_consumer(&peer_id, webrtcbin) {

                                                    gst::error!(CAT, obj: &element, "Failed to add consumer {} :{}", &peer_id, err);
    
                                                    if let Err(err) = element.remove_consumer(&peer_id, true, Some(err)) {
                                                        gst::warning!(CAT, obj: &element, "Failed to remove consumer {} :{}", &peer_id, err);
                                                    }
                                                }
                                            }
                                            Err(err) => {
                                                gst::error!(CAT, obj: &element, "{}", "Failed to create webrtcbin");

                                                if let Err(err) = element.remove_webrtcbin(&peer_id, Some(err)) {
    
                                                    gst::warning!(CAT, obj: &element, "Failed to remove webrtcbin: {}", err);
                                                }
                                            }
                                        }    
                                        
                                    }
                                    p::OutgoingMessage::EndSession { peer_id } => {
                                        if let Err(err) = element.remove_consumer(&peer_id, false, None) {
                                            gst::warning!(CAT, obj: &element, "{}", err);
                                        }
                                    }
                                    p::OutgoingMessage::Peer(p::PeerMessage {
                                        peer_id,
                                        peer_message,
                                    }) => match peer_message {
                                        p::PeerMessageInner::Sdp(p::SdpMessage::Answer { sdp }) => {
                                            match gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes()){
                                                Ok(sdp) => {
                                                    if let Err(err) = element.handle_sdp(
                                                        &peer_id,
                                                        &gst_webrtc::WebRTCSessionDescription::new(
                                                            gst_webrtc::WebRTCSDPType::Answer,
                                                            sdp,
                                                        ),
                                                    ) {
                                                        gst::error!(CAT, obj: &element, "Failed to handle sdp: {}", err);
                                                        if let Err(err) = element.remove_consumer(&peer_id, true,Some(err)) {
                                                            gst::warning!(CAT, obj: &element, "Failed to remove consumer {}: {}", &peer_id, err);
                                                        }
                                                    }
                                                },
                                                Err(err) => {
                                                    gst::error!(CAT, obj: &element, "Failed to handle sdp: {}", err);
                                                    let err = WebRTCSinkError::FailedNegotiate { details: "failed to parse sdp buffer".to_string(), peer_id: peer_id.clone() };
                                                    if let Err(err) = element.remove_consumer(&peer_id, true,Some(err)) {
                                                        gst::warning!(CAT, obj: &element, "Failed to remove consumer {}: {}", &peer_id, err);
                                                    }
                                                }
                                            }

                                            
                                        }
                                        p::PeerMessageInner::Sdp(p::SdpMessage::Offer {
                                            ..
                                        }) => {
                                            gst::warning!(
                                                CAT,
                                                obj: &element,
                                                "Ignoring offer from peer"
                                            );
                                        }
                                        p::PeerMessageInner::Ice {
                                            candidate,
                                            sdp_m_line_index,
                                        } => {
                                            if let Err(err) = element.handle_ice(
                                                &peer_id,
                                                Some(sdp_m_line_index),
                                                None,
                                                &candidate,
                                            ) {
                                                gst::warning!(CAT, obj: &element, "Failed to handle ice: {}", err);
                                                if let Err(err) = element.remove_consumer(&peer_id, true,Some(err)) {
                                                    gst::warning!(CAT, obj: &element, "Failed to remove consumer {}: {}", &peer_id, err);
                                                }
                                            }
                                        }
                                    },
                                    _ => {
                                        gst::warning!(
                                            CAT,
                                            obj: &element,
                                            "Ignoring unsupported message {:?}",
                                            msg
                                        );
                                    }
                                }
                            } else {
                                gst::error!(
                                    CAT,
                                    obj: &element,
                                    "Unknown message from server: {}",
                                    msg
                                );
                                element.handle_signalling_error(
                                    anyhow!("Unknown message from server: {}", msg).into(),
                                );
                            }
                        }
                        Ok(WsMessage::Close(reason)) => {
                            gst::info!(
                                CAT,
                                obj: &element,
                                "websocket connection closed: {:?}",
                                reason
                            );
                            break;
                        }
                        Ok(_) => (),
                        Err(err) => {
                            element.handle_signalling_error(
                                anyhow!("Error receiving: {}", err).into(),
                            );
                            break;
                        }
                    }
                } else {
                    break;
                }
            }

            if let Some(element) = element_clone.upgrade() {
                gst::info!(CAT, obj: &element, "Stopped websocket receiving");
            }
        });

        let mut state = match self.state.lock() {
            Ok(state) => Ok(state),
            Err(_) => Err(WebRTCSinkError::FailedLockMutex("failed to get state from mutex".to_string())),
        }?;
        state.websocket_sender = Some(websocket_sender);
        state.send_task_handle = Some(send_task_handle);
        state.receive_task_handle = Some(receive_task_handle);

        Ok(())
    }

    pub fn start(&self, element: &WebRTCSink) {
        let this = self.instance();
        let element_clone = element.clone();
        task::spawn(async move {
            let this = Self::from_instance(&this);
            if let Err(err) = this.connect(&element_clone).await {
                element_clone.handle_signalling_error(err.into());
            }
        });
    }

    pub fn handle_sdp(
        &self,
        element: &WebRTCSink,
        peer_id: &str,
        sdp: &gst_webrtc::WebRTCSessionDescription,
    ) -> Result<(),WebRTCSinkError> {
        let state = match self.state.lock() {
            Ok(state) => Ok(state),
            Err(_) => Err(WebRTCSinkError::FailedLockMutex("failed to get state from mutex".to_string())),
        }?;

        let sdp =  signaller_error_or_ok(sdp.sdp().as_text(), "failed to get sdp as text")?;

        let msg = p::IncomingMessage::Peer(p::PeerMessage {
            peer_id: peer_id.to_string(),
            peer_message: p::PeerMessageInner::Sdp(p::SdpMessage::Offer {
                sdp: sdp,
            }),
        });

        if let Some(mut sender) = state.websocket_sender.clone() {
            let element = element.downgrade();
            task::spawn(async move {
                if let Err(err) = sender.send(msg).await {
                    if let Some(element) = element.upgrade() {
                        element.handle_signalling_error(anyhow!("Error: {}", err).into());
                    }
                }
            });
        }

        Ok(())
    }

    pub fn handle_ice(
        &self,
        element: &WebRTCSink,
        peer_id: &str,
        candidate: &str,
        sdp_m_line_index: Option<u32>,
        _sdp_mid: Option<String>,
    )  -> Result<(),WebRTCSinkError> {
        let state = match self.state.lock() {
            Ok(state) => Ok(state),
            Err(_) => Err(WebRTCSinkError::FailedLockMutex("failed to get state from mutex".to_string())),
        }?;

        let sdp_m_line_index = signaller_some_or_none(sdp_m_line_index, "failed to get sdp m line index")?;
        let msg = p::IncomingMessage::Peer(p::PeerMessage {
            peer_id: peer_id.to_string(),
            peer_message: p::PeerMessageInner::Ice {
                candidate: candidate.to_string(),
                sdp_m_line_index: sdp_m_line_index,
            },
        });

        if let Some(mut sender) = state.websocket_sender.clone() {
            let element = element.downgrade();
            task::spawn(async move {
                if let Err(err) = sender.send(msg).await {
                    if let Some(element) = element.upgrade() {
                        element.handle_signalling_error(anyhow!("Error: {}", err).into());
                    }
                }
            });
        }
        Ok(())
    }

    pub fn stop(&self, element: &WebRTCSink) {
        gst::info!(CAT, obj: element, "Stopping now");

        if let Ok(mut state) = self.state.lock() {
            let send_task_handle = state.send_task_handle.take();
            let receive_task_handle = state.receive_task_handle.take();
            if let Some(mut sender) = state.websocket_sender.take() {
                task::block_on(async move {
                    sender.close_channel();
    
                    if let Some(handle) = send_task_handle {
                        if let Err(err) = handle.await {
                            gst::warning!(CAT, obj: element, "Error while joining send task: {}", err);
                        }
                    }
    
                    if let Some(handle) = receive_task_handle {
                        handle.await;
                    }
                });
            }
        }
        else{
            let err = WebRTCSinkError::FailedLockMutex("failed to get state from mutex".to_string());
            element.handle_signalling_error(Box::new(err));
        }
      
    }

    fn send_end_session_with_error(&self, element: &WebRTCSink, peer_id: &str, error: WebRTCSinkError){
        gst::info!(CAT, "Going to send error");
        if let Ok(state) = self.state.lock() {
            let peer_id = peer_id.to_string();
            let element = element.downgrade();
            if let Some(mut sender) = state.websocket_sender.clone() {
                task::spawn(async move {
                    if let Err(err) = sender
                        .send(p::IncomingMessage::EndSessionError(p::EndSessionErrorMessage {
                            peer_id: peer_id.to_string(),
                            error: error.to_string(),
                        }))
                        .await
                    {
                        if let Some(element) = element.upgrade() {
                            element.handle_signalling_error(anyhow!("Error: {}", err).into());
                        }
                    }
                });
            }
        }
        else{
            let err = WebRTCSinkError::FailedLockMutex("failed to get state from mutex".to_string());
            element.handle_signalling_error(Box::new(err));
        }
    }

    fn send_end_session(&self, element: &WebRTCSink, peer_id: &str){
        gst::info!(CAT, "Not going to send error");
        if let Ok(state) = self.state.lock() {
            let peer_id = peer_id.to_string();
            let element = element.downgrade();
            if let Some(mut sender) = state.websocket_sender.clone() {
                task::spawn(async move {
                    if let Err(err) = sender
                        .send(p::IncomingMessage::EndSession(p::EndSessionMessage {
                            peer_id: peer_id.to_string(),
                        }))
                        .await
                    {
                        if let Some(element) = element.upgrade() {
                            element.handle_signalling_error(anyhow!("Error: {}", err).into());
                        }
                    }
                });
            }
        }
        else{
            let err = WebRTCSinkError::FailedLockMutex("failed to get state from mutex".to_string());
            element.handle_signalling_error(Box::new(err));
        }
    }

    pub fn consumer_removed(&self, element: &WebRTCSink, peer_id: &str, error: Option<WebRTCSinkError>) {
        gst::debug!(CAT, obj: element, "Signalling consumer {} removed", peer_id);

        match error {
            Some(err) => self.send_end_session_with_error(element, peer_id, err),
            None => self.send_end_session(element, peer_id)
        };
    }
}

#[glib::object_subclass]
impl ObjectSubclass for Signaller {
    const NAME: &'static str = "RsWebRTCSinkSignaller";
    type Type = super::Signaller;
    type ParentType = glib::Object;
}

impl ObjectImpl for Signaller {
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPERTIES: Lazy<Vec<glib::ParamSpec>> = Lazy::new(|| {
            vec![
                glib::ParamSpecString::new(
                    "address",
                    "Address",
                    "Address of the signalling server",
                    Some("ws://127.0.0.1:8443"),
                    glib::ParamFlags::READWRITE,
                ),
                glib::ParamSpecString::new(
                    "cafile",
                    "CA file",
                    "Path to a Certificate file to add to the set of roots the TLS connector will trust",
                    None,
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
            "address" => {
                let address: Option<_> = value.get().expect("type checked upstream");

                if let Some(address) = address {
                    gst::info!(CAT, "Signaller address set to {}", address);

                    let mut settings = self.settings.lock().unwrap();
                    settings.address = Some(address);
                } else {
                    gst::error!(CAT, "address can't be None");
                }
            }
            "cafile" => {
                let value: String = value.get().unwrap();
                let mut settings = self.settings.lock().unwrap();
                settings.cafile = Some(value.into());
            }
            _ => unimplemented!(),
        }
    }

    fn property(&self, _obj: &Self::Type, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        match pspec.name() {
            "address" => self.settings.lock().unwrap().address.to_value(),
            "cafile" => {
                let settings = self.settings.lock().unwrap();
                let cafile = settings.cafile.as_ref();
                cafile.and_then(|file| file.to_str()).to_value()
            }
            _ => unimplemented!(),
        }
    }
}
