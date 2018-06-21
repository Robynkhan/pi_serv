use std::sync::Arc;
use std::io::{Error};
use std::ops::Deref;
use std::boxed::FnBox;

use pi_vm::pi_vm_impl::VMFactory;
use pi_lib::atom::Atom;
use pi_lib::sinfo::StructInfo;
use pi_lib::bon::{ReadBuffer, Decode};
use pi_db::memery_db::{MemeryDB};
use pi_db::mgr::Tr;
use pi_db::db::{TabKV, Iter, Ware};
use pi_db::mgr::Mgr;
use pi_base::timer::TIMER;
use rpc::server::RPCServer;
use rpc::traits::RPCServerTraits;
use net::{Config, Protocol};
use net::api::NetManager;
use mqtt::server::ServerNode;
use mqtt::data::Server;
use mqtt::session::Session;

use handler::TopicHandler;
use depend::Depend;
use init_js::push_pre;

//封装类db迭代器， 是其由traiobj转化为具体类型（构建工具暂时不支持traitobj的构建）
pub struct DBIter(Box<Iter>);

impl DBIter{
    pub fn next(&mut self, cb: Arc<Fn(Result<Option<(Arc<Vec<u8>>, Arc<Vec<u8>>)>, String>)>) -> Option<Result<Option<(Arc<Vec<u8>>, Arc<Vec<u8>>)>, String>>{
        match self.0.next(cb.clone()) {
            Some(v) => {cb(v); None},
            None => None,
        } 
    }
}

pub struct DBWare(Arc<Ware>);

// 取到数据库的迭代器
pub fn iter_db(tr: &Tr, ware: String, tab: String, key: Option<&[u8]>, descending: bool, _filter: Option<String>, cb: Arc<Fn(Result<DBIter, String>)>) -> Option<Result<DBIter, String>> {
    let key = match key {
        Some(v) => Some(Arc::new(Vec::from(v))),
        None => None,
    };

    let cb1 = move |r:Result<Box<Iter>, String>|{
        match r {
            Ok(v) => {cb(Ok(DBIter(v)))},
            Err(s) => {cb(Err(s))},
        }
    };
    let cb1 =  Arc::new(cb1);
    let ware = Atom::from(ware);
    let tab = Atom::from(tab);
    match tr.iter(&ware, &tab, key, descending, None, cb1.clone()) {
        Some(v) => {cb1(v); None},
        None => None,
    } 
}

// 注册数据库
pub fn register_db(mgr: &Mgr, prefix: String, ware: MemeryDB) -> bool {
	mgr.register(Atom::from(prefix), Arc::new(ware))
}

//创建一个Arc<StructInfo>
pub fn create_sinfo(data: &[u8]) -> Arc<StructInfo>{
	let mut buf = ReadBuffer::new(data, 0);
	Arc::new(StructInfo::decode(&mut buf))
}

//new TabKV
pub fn tabkv_with_value(ware: &str, tab: &str, key: &[u8], value: &[u8]) -> TabKV {
    TabKV{
        ware: Atom::from(ware),
        tab: Atom::from(tab),
        key: Arc::new(Vec::from(key)),
        index: 0,
        value: Some(Arc::new(Vec::from(value))),
    }
}

//new TabKV
pub fn tabkv_new(ware: &str, tab: &str, key: &[u8]) -> TabKV {
    TabKV{
        ware: Atom::from(ware),
        tab: Atom::from(tab),
        key: Arc::new(Vec::from(key)),
        index: 0,
        value: None,
    }
}

//TabKV get_value
pub fn tabkv_get_value(tabkv: &TabKV) -> Option<Arc<Vec<u8>>> {
    tabkv.value.clone()
}

//clone vm工厂（VMFactory没有显示实现clone方法， 无法导出， 需要封装）
pub fn clone_vm_factory(factory: &VMFactory) -> VMFactory{
    factory.clone()
}

//为mqtt绑定网络， 返回mqttserver
pub fn mqtt_bind(mgr: &NetManager, addr: String, protocol: String, send_buf_size: usize, recv_timeout: usize) -> ServerNode{
    let cfg = Config{
        protocol: match protocol.as_str() {
            "tcp" => Protocol::TCP,
            _ => {panic!("nonsupport protocol:{}", protocol);},
        },
        addr: addr.parse().unwrap()
    };
    let server = ServerNode::new();
    let copy = server.clone();
    mgr.bind(cfg, Box::new(move |peer, _addr| {
        let (socket, stream) = peer.unwrap();
        {
            let s = &mut stream.write().unwrap();

            s.set_close_callback(Box::new(|id, _reason| {
                println!("server handle_close, stream_id = {}",id);
            }));
            s.set_send_buf_size(send_buf_size);
            s.set_recv_timeout(recv_timeout);
        }
        let mut copy = copy.clone();
        copy.add_stream(socket, stream);
    }));
    server
}

pub fn mqtt_respond(session: Arc<Session>, topic: String, data: &[u8]) {
    println!("mqtt_respond------------------------{:p}", session);
    session.respond(Atom::from(topic), Vec::from(data));
}

//为rpc注册handler
pub fn register_rpc_handler(serv: &mut RPCServer, topic: String, sync: bool, handler: &Arc<TopicHandler>) -> Result<(), Error> {
    serv.register(Atom::from(topic), sync, handler.clone())
}

//new一个arc
pub fn arc_new<T>(v: T) -> Arc<T>{
    Arc::new(v)
}

//new一个arc
pub fn arc_deref< T>(v: &Arc<T>) -> &T{
    v.deref()
}

//new一个box
pub fn box_new<T>(v: T) -> Box<T>{
    Box::new(v)
}

//getdepend
pub fn get_depend(dp: &Depend, path: String) -> Vec<String> {
    let d = dp.depend(&[path]);
    let mut arr = Vec::new();
    for v in d.into_iter(){
        arr.push(v.borrow().path.clone());
    }
    push_pre(&mut arr);
    arr
}

//休眠
pub fn js_sleep(ms: u64, f: Box<FnBox()>){
	TIMER.set_timeout(f, ms);
}