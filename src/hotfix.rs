use std::sync::{Arc, RwLock, MutexGuard};
use std::mem::forget;
use std::env;
use std::path::PathBuf;

use fnv::FnvHashMap;

use pi_vm::pi_vm_impl::{ VMFactory };
use pi_vm::adapter::{ JS };
use pi_vm::bonmgr::{NativeObjsAuth};
use atom::Atom;

use file::fs_monitor::{FSMonitorOptions, FSListener, FSMonitor, FSChangeEvent};

use js_lib::JSGray;
use js_env::env_var;
use js_file::read_file_string_sync;
use js_net::{ RequestHandler, create_rpc_service, register_rpc_topic };
use init_js::{read_code, load_core_env};


lazy_static! {
    // 灰度表
    pub static ref GRAY_TABLE: Arc<RwLock<GrayTable>> = Arc::new(RwLock::new(GrayTable::new()));
    // 每个灰度版本对应的字节码列表
    pub static ref BYTE_CODE_CACHE: Arc<RwLock<Vec<FnvHashMap<String, Arc<Vec<u8>>>>>> = Arc::new(RwLock::new(vec![FnvHashMap::default()]));
}

pub fn get_gray_table() -> Arc<RwLock<GrayTable>> {
    GRAY_TABLE.clone()
}

pub fn register_jsgray(gray_tab: Arc<RwLock<GrayTable>>, version: Option<usize>, jsgray: JSGray) {
    let mut gray_tab = gray_tab.write().unwrap();
    let name = jsgray.name.clone();
    match version {
        Some(ver) => {
            match gray_tab.jsgrays.get_mut(ver) {
                Some(gray) => {
                    gray.insert(name, Arc::new(jsgray));
                }
                None => {
                    panic!("version not found {:?}", version);
                }
            }
        }
        None => {
            match gray_tab.jsgrays.last_mut() {
                Some(gray) => {
                    gray.insert(name, Arc::new(jsgray));
                }
                None => {
                    let mut map = FnvHashMap::default();
                    map.insert(name, Arc::new(jsgray));
                    gray_tab.jsgrays.push(map);
                }
            }
        }
    }
}

// 提升灰度版本号，相应的克隆字节码和jsgray
fn bump_gray_version() {
    let mut gray_tab = GRAY_TABLE.write().unwrap();
    let mut map = FnvHashMap::default();
    match BYTE_CODE_CACHE.read().unwrap().last() {
        Some(byte_codes) => {
            for (k, v) in byte_codes.iter() {
                map.insert(k.clone(), v.clone());
            }
        }
        None => {}
    }

    BYTE_CODE_CACHE.write().unwrap().push(map);

    let mut map2 = FnvHashMap::default();
    match gray_tab.jsgrays.last() {
        Some(jsgray) => {
            for (k, v) in jsgray.iter() {
                map2.insert(k.clone(), v.clone());
            }
            gray_tab.jsgrays.push(map2);
        }

        None => {}
    }
}

pub fn get_byte_code(mod_id: String) -> Option<Arc<Vec<u8>>> {
    BYTE_CODE_CACHE.read().unwrap().last().unwrap().get(&mod_id).cloned()
}

pub fn remove_byte_code(mod_id: String) {
    BYTE_CODE_CACHE.write().unwrap().last_mut().unwrap().remove(&mod_id);
}

pub fn compile_byte_code(name: String, source_code: String) -> Option<Arc< Vec<u8>>> {
    let opts = JS::new(1, Atom::from("compile"), Arc::new(NativeObjsAuth::new(None, None)), None).unwrap();
	match opts.compile(name.clone(), source_code) {
		Some(r) => {
            BYTE_CODE_CACHE.write().unwrap().last_mut().unwrap().insert(name, Arc::new(r.clone()));
            Some(Arc::new(r))
		}
		None => None,
	}
}

pub struct GrayTable {
    // 每个灰度版本的所有 jsgray
    pub jsgrays: Vec<FnvHashMap<Atom, Arc<JSGray>>>,
}

impl GrayTable {
    pub fn new() -> Self {
        GrayTable {
            jsgrays: vec![FnvHashMap::default()],
        }
    }
}

pub fn gray_table_to_arc(gray_tab: GrayTable) -> Arc<RwLock<GrayTable>> {
    Arc::new(RwLock::new(gray_tab))
}


pub fn hotfix_listen(path: String) {
    let listener = FSListener(Arc::new(move |event: FSChangeEvent| {
        match event {
            FSChangeEvent::Create(path) => {
                // 创建新的模块，其他地方引入时会自己 require
                // 需要处理创建的是 .event.js 文件的情况
                let mod_id = normalize_module_id(path.to_str().unwrap());
                if mod_id.ends_with(".event.js") {
                    debug!("create mod id {:?}", mod_id);
                    let auth = Arc::new(NativeObjsAuth::new(None, None));
                    let js = JS::new(1, Atom::from("hotfix compile"), auth.clone(), None).unwrap();
                    load_core_env(&js);

                    let mgr = GRAY_TABLE.read().unwrap().jsgrays.last().unwrap().values().take(1).next().unwrap().mgr.clone();

                    let cur_exe = env::current_exe().unwrap();
                    let env_code = read_code(&cur_exe.join("../env.js"));
                    let core_code = read_code(&cur_exe.join("../core.js"));

                    let env_code = js.compile("env.js".to_string(), env_code).unwrap();
                    let core_code = js.compile("core.js".to_string(), core_code).unwrap();

                    let mut vmf = VMFactory::new(&mod_id, 128, 2, 33554432, 33554432, auth);

                    // env.js / core.js 代码
                    vmf = vmf.append(Arc::new(env_code));
                    vmf = vmf.append(Arc::new(core_code));

                    let rpc_boot_code = "pi_pt/net/rpc_entrance.js";

                    let extra_code = format!("Module.require(\'{}\', '');", rpc_boot_code);
                    let extra_code = extra_code + format!("Module.require(\'{}\', '');", mod_id).as_str();
                    let extra_code = js.compile("rpc_entrance".to_string(), extra_code).unwrap();

                    // rpc 功能依赖的代码，和实际处理rpc需要的代码
                    vmf = vmf.append(Arc::new(extra_code));
                    vmf.produce(2);

                    let jsgray = JSGray::new(&mgr, Arc::new(vmf), mod_id.as_str());
                    register_jsgray(get_gray_table(), None, jsgray);
                    let handler = RequestHandler::new(&get_gray_table());
                    let service = create_rpc_service(&handler);

                    // 读取.event.js 文件编译生成的 .cfg.json 文件， 解析topic名字

                    let json_file = mod_id.as_str().split(".").collect::<Vec<&str>>()[0].to_string() + "_event.cfg.json";
                    if let Ok(jstr) = read_file_string_sync(&json_file) {
                        let mut parsed = json::parse(&jstr).unwrap();
                        for s in parsed["pi_sys/modules/event/entrance.Entrance"].members_mut() {
                            register_rpc_topic(s[0].take_string().unwrap() , &service);
                        }
                    } else {
                        println!("not found json file");
                    }
                }
            },
            // 每次文件改变都会增加一个灰度版本号
            FSChangeEvent::Write(path) => {
                let mod_id = normalize_module_id(path.to_str().unwrap());
                if mod_id.ends_with(".js") {
                    bump_gray_version();
                    module_changed(path);
                }
            },
            FSChangeEvent::Remove(path) => {
                let mod_id = normalize_module_id(path.to_str().unwrap());
                if mod_id.ends_with(".js") {
                    bump_gray_version();
                    if mod_id.ends_with(".event.js") {
                        // 如果删除的是 .event.js 结尾的模块，那么删除对应的虚拟机工厂
                        match GRAY_TABLE.write().unwrap().jsgrays.last_mut().unwrap().remove(&Atom::from(mod_id.clone())) {
                            Some(_) => debug!("remove factory success : {:?}", mod_id),
                            None => debug!("{:?} factory not exist", mod_id)
                        }
                    }
                    module_changed(path);
                }
            },
            FSChangeEvent::Rename(old, new) => {
                // 名字的变更会引起引入名字的变化，不需要处理
            },
        };
    }));
    let mut monitor = FSMonitor::new(FSMonitorOptions::Dir(Atom::from(path), true, 1000), listener);
    monitor.run().expect("watch dir failed");
    forget(monitor);
}

fn module_changed(path: PathBuf) {
    let auth = Arc::new(NativeObjsAuth::new(None, None));
    let js = JS::new(1, Atom::from("hotfix compile"), auth.clone(), None).unwrap();
    load_core_env(&js);

    let mod_id = normalize_module_id(path.to_str().unwrap());

    let mut gray_tab = GRAY_TABLE.write().unwrap();
    let mut jsgrays = gray_tab.jsgrays.last_mut().unwrap();

    for (k, v) in jsgrays.iter_mut() {
        if is_depend(&js, k.as_str(), &mod_id) {
            debug!("{:?} is a depend for {:?}", mod_id, k);
            let cur_exe = env::current_exe().unwrap();
            let env_code = read_code(&cur_exe.join("../env.js"));
            let core_code = read_code(&cur_exe.join("../core.js"));

            let env_code = js.compile("env.js".to_string(), env_code).unwrap();
            let core_code = js.compile("core.js".to_string(), core_code).unwrap();

            let mgr = v.mgr.clone();
            let auth = Arc::new(NativeObjsAuth::new(None, None));
            let mut vmf = VMFactory::new(k.as_str(), 128, 2, 33554432, 33554432, auth);

            // env.js / core.js 代码
            vmf = vmf.append(Arc::new(env_code));
            vmf = vmf.append(Arc::new(core_code));

            let rpc_boot_code = "pi_pt/net/rpc_entrance.js";

            remove_byte_code(mod_id.clone());

            let extra_code = format!("Module.require(\'{}\', '');", rpc_boot_code);
            let extra_code = extra_code + format!("Module.require(\'{}\', '');", k.clone().to_string()).as_str();
            let extra_code = js.compile("rpc_entrance".to_string(), extra_code).unwrap();

            // rpc 功能依赖的代码，和实际处理rpc需要的代码
            vmf = vmf.append(Arc::new(extra_code));
            vmf.produce(2);

            let jsgray = JSGray::new(&mgr, Arc::new(vmf), k.as_str());

            // 用新的代码替换
            *v = Arc::new(jsgray);
        } else {

        }
    }
}

fn is_depend(js: &Arc<JS>, vmf_name: &str, mod_id: &str) -> bool {
    let cur_dir = env_var("PROJECT_ROOT").unwrap();

    if js.get_link_function("Module.require".to_string()) {
        js.new_str(vmf_name.clone().to_string()).unwrap();
        js.new_str(cur_dir).unwrap();
        js.call(2);
    } else {
        panic!("Module.require function is not exist");
    }

    js.get_js_function("isDepend".to_string());
    js.new_str(mod_id.clone().to_string());
    let ret = js.invoke(1);
    if ret.get_boolean() {
        return true;
    } else {
        return false;
    }
}

fn normalize_module_id(mod_id: &str) -> String {
    mod_id.replace("\\", "/")
        .as_str()
        .trim_start_matches(&(env_var("PROJECT_ROOT").unwrap() + "/"))
        .to_string()
}


