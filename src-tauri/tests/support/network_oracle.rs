//! Actual pinned MAIN.IP + getLocalIPs, with real native OS data and bounded Node.
use serde_json::Value;
use std::{
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
pub fn capture(payloads: &[Value]) -> Value {
    let source =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/electron/data/bonjour.ts");
    let source = if source.exists() {
        source
    } else {
        PathBuf::from(file!())
            .parent()
            .unwrap()
            .join("../../../src/electron/data/bonjour.ts")
    };
    let root = source
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let script = r#"const fs=require('fs'),os=require('os'),vm=require('vm'),crypto=require('crypto'),ts=require('typescript');
const read=(file,sha)=>{const text=fs.readFileSync(file,'utf8').replaceAll('\r\n','\n');if(crypto.createHash('sha256').update(text).digest('hex')!==sha)throw Error('immutable source mismatch: '+file);return ts.createSourceFile(file,text,ts.ScriptTarget.Latest,true)};
const ast=read(process.argv[1],'6ff78829b6f8a383d7584fdbec69e134892bb3299fbd7189901e41bcd4c5e869');const fn=ast.statements.find(n=>ts.isFunctionDeclaration(n)&&n.name.text==='getLocalIPs');
const main=read(require('path').join(process.cwd(),'src/electron/IPC/responsesMain.ts'),'70372b48b698f74f239b82f92e48dde69880a7c4d78af1a9f0e1723313fb57a3');const list=main.statements.find(n=>ts.isVariableStatement(n)&&n.declarationList.declarations.some(d=>d.name.getText(main)==='mainResponses'));const object=list.declarationList.declarations.find(d=>d.name.getText(main)==='mainResponses').initializer;const handler=object.properties.find(p=>p.name?.getText(main)==='[Main.IP]').initializer.getText(main);
const interfaces=os.networkInterfaces();const context=vm.createContext({os:{networkInterfaces:()=>interfaces,hostname:()=>{throw Error('MAIN.IP must not read hostname')}}});const code=fn.getText(ast).replace(/^export\s+/,'')+'; globalThis.load = '+handler;new vm.Script(ts.transpileModule(code,{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText).runInContext(context);
const payloads=JSON.parse(process.argv[2]);console.log(JSON.stringify({runtime:{node:process.version,uv:process.versions.uv},hostname:os.hostname(),interfaces:Object.entries(interfaces).map(([name,addresses])=>({name,addresses:addresses.map(({address,family,internal,mac})=>({address,family,internal,mac}))})),replies:payloads.map(payload=>context.load(payload))}));"#;
    let stdout = tempfile::NamedTempFile::new().unwrap();
    let stderr = tempfile::NamedTempFile::new().unwrap();
    let node = std::env::var_os("FREESHOW_TEST_NODE").unwrap_or_else(|| "node".into());
    let mut child = Command::new(node)
        .args(["-e", script])
        .arg(&source)
        .arg(serde_json::to_string(payloads).unwrap())
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(stdout.reopen().unwrap())
        .stderr(stderr.reopen().unwrap())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline
            || stdout.as_file().metadata().unwrap().len() > 1024 * 1024
            || stderr.as_file().metadata().unwrap().len() > 1024 * 1024
        {
            let _ = child.kill();
            let _ = child.wait();
            panic!("bounded native source oracle timed out/overflowed");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(stdout.as_file().metadata().unwrap().len() <= 1024 * 1024);
    assert!(stderr.as_file().metadata().unwrap().len() <= 1024 * 1024);
    let mut out = String::new();
    stdout.as_file().read_to_string(&mut out).unwrap();
    let mut err = String::new();
    stderr.as_file().read_to_string(&mut err).unwrap();
    assert!(status.success(), "{err}");
    serde_json::from_str(&out).unwrap()
}
