use async_raft::{InitializeError, RaftError};
use camelpaste::paste;
use prost::DecodeError;
use qp2p::{EndpointError, SendError};
use thiserror::Error;

use crate::{sys::NodeID, util::TryUtf8VecU8};

pub type WSResult<T> = Result<T, WSError>;

#[derive(Debug)]
pub enum NotMatchNodeErr {
    NotRaft(String),
}

#[derive(Debug)]
pub enum WsNetworkLogicErr {
    DecodeError(DecodeError),
    MsgIdNotDispatchable(u32),
    InvaidNodeID(NodeID),
}

#[derive(Debug)]
pub enum WsNetworkConnErr {
    EndPointError(EndpointError),
    SendError(SendError),
    ConnectionNotEstablished(NodeID),
    RPCTimout(NodeID),
    ConnectionExpired(NodeID),
}

#[derive(Debug)]
pub enum WsIoErr {
    Io(std::io::Error),
}

#[derive(Debug)]
pub enum WsRaftErr {
    InitializeError(async_raft::error::InitializeError),
    RaftError(RaftError),
}

#[derive(Debug)]
pub enum WsSerialErr {
    BincodeErr(Box<bincode::ErrorKind>),
    AppMetaKvKeyIndexOutOfBound {
        app: String,
        func: String,
        index: usize,
        kvs_len: Option<usize>,
    },
}

#[derive(Error, Debug)]
pub enum WsFormatErr {
    #[error("KeyPatternFormatErr: {key_pattern}, '{{' '}}' should appear in pair, only '_' is allowed to appear as symbol")]
    KeyPatternFormatErr { key_pattern: String },
}

#[derive(Debug)]
pub enum WsPermissionErr {
    AccessKeyPermissionDenied {
        app: String,
        func: String,
        access_key: TryUtf8VecU8,
    },
}

#[derive(Error, Debug)]
pub enum WSError {
    #[error("Io error: {0:?}")]
    WsIoErr(WsIoErr),

    #[error("Network logic error: {0:?}")]
    WsNetworkLogicErr(WsNetworkLogicErr),

    #[error("Network connection error: {0:?}")]
    WsNetworkConnErr(WsNetworkConnErr),

    #[error("Raft error: {0:?}")]
    WsRaftErr(WsRaftErr),

    #[error("Permission error: {0:?}")]
    WsPermissionErr(WsPermissionErr),

    #[error("Serial error: {0:?}")]
    WsSerialErr(WsSerialErr),

    #[error("Format error: {0:?}")]
    WsFormatErr(WsFormatErr),

    #[error("Not Implemented")]
    NotImplemented,
}

impl From<WsNetworkLogicErr> for WSError {
    fn from(e: WsNetworkLogicErr) -> Self {
        WSError::WsNetworkLogicErr(e)
    }
}

impl From<WsNetworkConnErr> for WSError {
    fn from(e: WsNetworkConnErr) -> Self {
        WSError::WsNetworkConnErr(e)
    }
}

impl From<WsRaftErr> for WSError {
    fn from(e: WsRaftErr) -> Self {
        WSError::WsRaftErr(e)
    }
}

impl From<WsSerialErr> for WSError {
    fn from(e: WsSerialErr) -> Self {
        WSError::WsSerialErr(e)
    }
}

impl From<WsIoErr> for WSError {
    fn from(e: WsIoErr) -> Self {
        WSError::WsIoErr(e)
    }
}

impl From<WsPermissionErr> for WSError {
    fn from(e: WsPermissionErr) -> Self {
        WSError::WsPermissionErr(e)
    }
}

pub struct ErrCvt<T>(pub T);

macro_rules! impl_err_convertor {
    ($t:ty,$sub_t:ty,$sub_tt:ty) => {
        paste! {
            impl ErrCvt<$t> {
                pub fn [<to_ $sub_t:snake>](self) -> WSError {
                    WSError::$sub_t($sub_t::$sub_tt(self.0))
                }
            }
        }
    };
}

impl_err_convertor!(DecodeError, WsNetworkLogicErr, DecodeError);
impl_err_convertor!(EndpointError, WsNetworkConnErr, EndPointError);
impl_err_convertor!(SendError, WsNetworkConnErr, SendError);
impl_err_convertor!(InitializeError, WsRaftErr, InitializeError);
impl_err_convertor!(RaftError, WsRaftErr, RaftError);
impl_err_convertor!(std::io::Error, WsIoErr, Io);
