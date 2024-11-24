use async_trait::async_trait;
use enum_as_inner::EnumAsInner;
use lazy_static::lazy_static;
use parking_lot::RwLock;
use prost::{
    bytes::{BufMut, BytesMut},
    Message,
};
use std::{
    collections::HashMap,
    hash::Hash,
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};
use tokio::{net::UnixListener, sync::oneshot};

use crate::result::{WSResult, WsRpcErr};

// start from the begining
#[async_trait]
pub trait RpcCustom: Sized + 'static {
    type SpawnArgs: Send + 'static;

    fn bind(a: Self::SpawnArgs) -> UnixListener;
    // return true if the id matches remote call pack
    fn handle_remote_call(conn: &HashValue, id: u8, buf: &[u8]) -> bool;
    async fn verify(buf: &[u8]) -> Option<HashValue>;
    // fn deserialize(id: u16, buf: &[u8]);
}

pub fn spawn<R: RpcCustom>(a: R::SpawnArgs) -> tokio::task::JoinHandle<()> {
    tokio::spawn(accept_task::<R>(a))
}

async fn accept_task<R: RpcCustom>(a: R::SpawnArgs) {
    // std::fs::remove_file(AGENT_SOCK_PATH).unwrap();

    // clean_sock_file(AGENT_SOCK_PATH);
    // let listener = tokio::net::UnixListener::bind(AGENT_SOCK_PATH).unwrap();

    let listener = R::bind(a);
    loop {
        let (socket, _) = listener.accept().await.unwrap();
        let _ = tokio::spawn(listen_task::<R>(socket));
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, EnumAsInner)]
pub enum HashValue {
    Int(i64),
    Str(String),
}

pub trait MsgIdBind: Sized + Message + Default + 'static {
    fn id() -> u16;
}

pub trait ReqMsg: MsgIdBind {
    type Resp: MsgIdBind;
}

pub fn close_conn(id: &HashValue) {
    // this will make the old receive loop to be closed
    let _ = CONN_MAP.write().remove(id);
}

pub async fn call<Req: ReqMsg>(
    req: Req,
    conn: HashValue,
    timeout: Duration,
) -> WSResult<Req::Resp> {
    // wait for connection if not connected

    let tx = {
        let mut conn_map = CONN_MAP.write();
        match conn_map.get_mut(&conn) {
            None => {
                return Err(WsRpcErr::ConnectionNotEstablished(conn).into());
            }
            Some(state) => {
                // Directly return the existing sender in the connected state
                state.tx.clone()
            }
        }
    };

    // register the call back
    let (wait_tx, wait_rx) = oneshot::channel();
    let next_task = NEXT_TASK_ID.fetch_add(1, Ordering::SeqCst);
    tracing::debug!("insert into CALL_MAP next_task:{:?}", next_task.clone());
    let _ = CALL_MAP.write().insert(next_task, wait_tx);
    tracing::debug!("insert after CALL_MAP.write(): {:?}", CALL_MAP.write());

    // send the request
    let mut buf = BytesMut::with_capacity(req.encoded_len() + 8);
    buf.put_i32(req.encoded_len() as i32);
    buf.put_i32(next_task as i32);
    buf.put_i32(2);
    req.encode(&mut buf).unwrap();

    tracing::debug!("send request: {:?} with len: {}", req, buf.len() - 8);
    tx.send(buf.into()).await.unwrap();

    // if timeout, take out the registered channel tx, return error
    let channel_resp = match tokio::time::timeout(timeout, wait_rx).await {
        Ok(ok) => ok,
        Err(_) => {
            //remove the cb channel
            tracing::debug!("call timeout: {:?}", req);
            assert!(CALL_MAP.write().remove(&next_task).is_some());
            return Err(WsRpcErr::RPCTimout(conn).into());
        }
    }
    .expect("cb channel must has a value if not timeout");

    match Req::Resp::decode(&mut channel_resp.as_slice()) {
        Ok(ok) => Ok(ok),
        Err(_err) => Err(WsRpcErr::InvalidMsgData { msg: Box::new(req) }.into()),
    }
}

// pub enum ConnState {
//     Connecting,
//     Connected(tokio::sync::mpsc::Sender<()>),
//     Disconnected,
// }

#[derive(Debug)]
pub struct ConnState {
    /// record the waiters
    // Connecting(Vec<oneshot::Sender<tokio::sync::mpsc::Sender<Vec<u8>>>>),
    pub tx: tokio::sync::mpsc::Sender<Vec<u8>>,
}

lazy_static! {
    /// This is an example for using doc comment attributes
    pub static ref CONN_MAP: RwLock<HashMap<HashValue,ConnState>> = RwLock::new(HashMap::new());

    static ref CALL_MAP: RwLock<HashMap<u32,oneshot::Sender<Vec<u8>>>> = RwLock::new(HashMap::new());

    static ref NEXT_TASK_ID: AtomicU32 = AtomicU32::new(0);
}

async fn listen_task<R: RpcCustom>(socket: tokio::net::UnixStream) {
    tracing::debug!("new connection: {:?}", socket.peer_addr().unwrap());
    let (mut sockrx, socktx) = socket.into_split();

    let mut buf = [0; 1024];
    let mut len = 0;

    let Some((conn, rx)) =
        listen_task_ext::verify_remote::<R>(&mut sockrx, &mut len, &mut buf).await
    else {
        tracing::warn!("verify failed");
        return;
    };

    tracing::debug!("verify_remote 结束");

    listen_task_ext::spawn_send_loop(rx, socktx);

    tracing::debug!("spawn_send_loop 结束");

    listen_task_ext::read_loop::<R>(conn, &mut sockrx, &mut len, &mut buf).await;

    tracing::debug!("read_loop 结束");
}

pub(super) mod listen_task_ext {
    use bytes::{Buf, Bytes};
    use prost::bytes;
    use std::time::Duration;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::unix::{OwnedReadHalf, OwnedWriteHalf},
        sync::mpsc::Receiver,
    };

    use crate::general::network::rpc_model::ConnState;

    use super::{HashValue, RpcCustom, CALL_MAP, CONN_MAP};

    pub(super) async fn verify_remote<R: RpcCustom>(
        sockrx: &mut OwnedReadHalf,
        len: &mut usize,   // 0
        buf: &mut [u8],    // 0
    ) -> Option<(HashValue, Receiver<Vec<u8>>)> {
        async fn verify_remote_inner<R: RpcCustom>(
            sockrx: &mut OwnedReadHalf,
            len: &mut usize,
            buf: &mut [u8],
        ) -> Option<(HashValue, Receiver<Vec<u8>>)> {
            // println!("waiting for verify head len");
            if !wait_for_len(sockrx, len, 4, buf).await {
                tracing::warn!("failed to read verify head len");
                return None;
            }

            let verify_msg_len = consume_i32(0, buf, len);

            tracing::debug!("len: {}, verify_msg_len: {}", len, verify_msg_len);

            // println!("waiting for verify msg {}", verify_msg_len);
            if !wait_for_len(sockrx, len, verify_msg_len, buf).await {
                tracing::warn!("failed to read verify msg");
                return None;
            }
            // println!("wait done");

            tracing::debug!("wait_for_len 完成");

            let Some(id) = R::verify(&buf[4..4 + verify_msg_len]).await else {
                tracing::warn!("verify failed in verify_remote_inner");
                return None;
            };
            let (tx, rx) = tokio::sync::mpsc::channel(10);

            // 确定一下为什么 conn_map 里面有上一次连接 id， 需要找这个 conn_map 在哪里都被调用了
            let mut write_conn_map = CONN_MAP.write();
            tracing::debug!("write_conn_map: {:?}", write_conn_map);
            if write_conn_map.contains_key(&id) {
                tracing::warn!("conflict conn id: {:?}", id);
                return None;
            }
            let _ = write_conn_map.insert(id.clone(), ConnState { tx });
            tracing::debug!("insert into CALL_MAP id:{:?}", id.clone());
            tracing::debug!("insert after CALL_MAP.write(): {:?}", write_conn_map);

            // println!("verify success");
            Some((id, rx))
        }
        let res = tokio::time::timeout(
            Duration::from_secs(5),
            verify_remote_inner::<R>(sockrx, len, buf),
        )
        .await
        .unwrap_or_else(|_elapse| None);
        // println!("verify return");
        res
    }

    pub(super) async fn read_loop<R: RpcCustom>(
        conn: super::HashValue,
        socket: &mut OwnedReadHalf,
        len: &mut usize,
        buf: &mut [u8],
    ) {
        *len = 0;
        let mut offset = 0;
        loop {

            let (msg_len, msg_id, taskid) = {
                let buf = &mut buf[offset..];
                if !wait_for_len(socket, len, 9, buf).await {
                    tracing::warn!("failed to read head len, stop rd loop");
                    return;
                }
                offset += 9;
                (
                    consume_i32(0, buf, len),
                    consume_u8(4, buf, len),
                    consume_i32(5, buf, len) as u32,
                )
            };
            
            tracing::debug!("2 len: {}, msg_len: {}, msg_id: {}, taskid: {}", len, msg_len, msg_id, taskid);

            {
                if buf.len() < offset + msg_len {
                    // move forward
                    buf.copy_within(offset.., 0);
                    offset = 0;
                }
                let buf = &mut buf[offset..];

                if !wait_for_len(socket, len, msg_len, buf).await {
                    tracing::warn!("failed to read head len, stop rd loop");
                    return;
                }

                if !R::handle_remote_call(&conn, msg_id, &buf[..msg_len]) {
                    // call back
                    let Some(cb) = CALL_MAP.write().remove(&taskid) else {
                        tracing::warn!(
                            "rd stream is not in correct format, taskid:{} msgid:{}",
                            taskid,
                            msg_id
                        );
                        return;
                    };

                    let msg = buf[..msg_len].to_vec();
                    tracing::debug!("msg: {:?}", msg);
                    cb.send(msg).unwrap();
                }

                // update the buf meta
                offset += msg_len;
                *len -= msg_len;

                tracing::debug!("1 len: {}, msg_len: {}, msg_id: {}, taskid: {}", len, msg_len, msg_id, taskid);
            }

            // match socket.read(buf).await {
            //     Ok(n) => {
            //         if n == 0 {
            //             tracing::warn!("connection closed");
            //             return;
            //         }
            //         // println!("recv: {:?}", buf[..n]);
            //         *len += n;
            //     }
            //     Err(e) => {
            //         println!("failed to read from socket; err = {:?}", e);
            //         return;
            //     }
            // }
        }
    }

    pub fn spawn_send_loop(
        mut rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
        mut socktx: OwnedWriteHalf,
    ) {
        let _ = tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Some(res) => {
                        socktx.write_all(&res).await.unwrap();
                    }
                    None => {
                        tracing::debug!(
                            "the old rd loop closed: {:?}",
                            socktx.peer_addr().unwrap()
                        );
                        break;
                    }
                }
            }
        });
    }

    pub(super) async fn wait_for_len(
        socket: &mut OwnedReadHalf,
        len: &mut usize,
        tarlen: usize,
        buf: &mut [u8],
    ) -> bool {
        while *len < tarlen {
            tracing::debug!("current len: {}, target len: {}", *len, tarlen);
            match socket.read(buf).await {
                Ok(n) => {
                    if n == 0 {
                        tracing::warn!("connection closed");
                        return false;
                    }
                    // println!("recv: {:?}", buf[..n]);
                    tracing::debug!("len += {}", n);
                    *len += n;
                }
                Err(e) => {
                    tracing::warn!("failed to read from socket; err = {:?}", e);
                    return false;
                }
            }
        }
        true
    }

    pub(super) fn consume_u8(off: usize, buf: &mut [u8], len: &mut usize) -> u8 {
        // let ret = bincode::deserialize::<i32>(&buf[off..off + 4]).unwrap() as usize;
        *len -= 1;
        buf[off]
    }

    // 4字节u32 长度
    pub(super) fn consume_i32(off: usize, buf: &mut [u8], len: &mut usize) -> usize {
        // let ret = bincode::deserialize::<i32>(&buf[off..off + 4]).unwrap() as usize;
        let ret = Bytes::copy_from_slice(&buf[off..off + 4]).get_i32();
        *len -= 4;
        ret as usize
    }
}
