"""One native user path for Tskmgr, using exclusively owned process/window targets.

Includes real WMI details, title search, TCP/UDP view, hung-window feedback,
activation, close, and explicit end/cancel. Raw screenshots remain private.
"""
import argparse,ctypes as c,json,os,subprocess,sys,time,socket
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(ROOT/'scripts'))
import settings_probe as p,scenario_probe as s
p.u.SetWindowTextW.argtypes=[c.wintypes.HWND,c.wintypes.LPCWSTR]
p.u.EnumWindows.argtypes=[c.c_void_p,c.wintypes.LPARAM]
from ctypes import wintypes as w
fixture_mode=len(sys.argv)>1 and sys.argv[1]=='--fixture'
if fixture_mode:
 out=Path(sys.argv[3]).parent
else:
 parser=argparse.ArgumentParser(description='Focused native task-tool smoke; owned targets only, no UAC or shell replacement.')
 parser.add_argument('--candidate',type=Path,required=True)
 parser.add_argument('--section',choices=['all','end'],default='all',help='Resume only process end/cancel after unrelated paths have accepted evidence')
 args=parser.parse_args();exe=args.candidate.resolve(strict=True)
 out=ROOT/'target/test-reports/tskmgr-ui'/str(time.time_ns());out.mkdir(parents=True)
if len(sys.argv)>1 and sys.argv[1]=='--fixture':
 tag=sys.argv[2];ready=Path(sys.argv[3]);desktop=s.Desktop()
 PROC=c.WINFUNCTYPE(c.c_ssize_t,w.HWND,w.UINT,w.WPARAM,w.LPARAM)
 p.u.DefWindowProcW.argtypes=[w.HWND,w.UINT,w.WPARAM,w.LPARAM];p.u.DefWindowProcW.restype=c.c_ssize_t
 p.u.DestroyWindow.argtypes=[w.HWND]
 def proc(h,m,a,b):
  if m==0x800a:
   ready.with_suffix(".blocked").write_text("");time.sleep(2);ready.with_suffix(".resumed").write_text("");return 0
  if m==16:p.u.DestroyWindow(h);return 0
  if m==2:p.u.PostQuitMessage(0);return 0
  return p.u.DefWindowProcW(h,m,a,b)
 callback=PROC(proc)
 class WC(c.Structure):
  _fields_=[('style',w.UINT),('proc',PROC),('ce',c.c_int),('we',c.c_int),('inst',w.HINSTANCE),('icon',w.HICON),('cursor',w.HANDLE),('brush',w.HBRUSH),('menu',w.LPCWSTR),('name',w.LPCWSTR)]
 p.k.GetModuleHandleW.argtypes=[w.LPCWSTR];p.k.GetModuleHandleW.restype=w.HINSTANCE
 p.u.RegisterClassW.argtypes=[c.POINTER(WC)];p.u.RegisterClassW.restype=w.WORD
 name='tskmgr_owned_'+str(os.getpid());wc=WC(0,callback,0,0,p.k.GetModuleHandleW(None),None,None,w.HBRUSH(6),None,name)
 assert p.u.RegisterClassW(c.byref(wc))
 hwnd=p.u.CreateWindowExW(0,name,tag,0x10cf0000,70,90,340,160,None,None,wc.inst,None);assert hwnd
 tcp=socket.socket();tcp.bind(('127.0.0.1',0));tcp.listen(1)
 udp=socket.socket(type=socket.SOCK_DGRAM);udp.bind(('127.0.0.1',0))
 ready.write_text(json.dumps(dict(pid=os.getpid(),hwnd=hwnd)))
 msg=w.MSG()
 while p.u.GetMessageW(c.byref(msg),None,0,0)>0:p.u.TranslateMessage(c.byref(msg));p.u.DispatchMessageW(c.byref(msg))
 sys.exit(0)

desktop=s.Desktop();original=p.u.GetForegroundWindow();shell=p.shell_settings();area=desktop.workarea();original_winmgr=p.pid(p.message())
children=[];tool=None;hwnd=None;report={'passed':False,'binary_sha256':s.digest(exe)}
def wait(f,timeout=12):return p.wait(f,timeout=timeout)
def ctl(i):return p.u.GetDlgItem(hwnd,i)
def txt(i):return p.text(ctl(i))
def click(i):
 wait(lambda:p.u.IsWindowEnabled(ctl(i)))
 p.u.SendMessageW(ctl(i),0xf5,0,0)
def main_window(pid):
 rows=[]
 cb=c.WINFUNCTYPE(w.BOOL,w.HWND,w.LPARAM)(lambda h,_:(rows.append(h) if p.pid(h)==pid and p.u.IsWindowVisible(h) and 'minicon-tskmgr' in p.text(h) else None) or True)
 p.u.EnumWindows(cb,0);return rows[0] if rows else None

def fixture(letter):
 tag='TSKMGR OWNED FIXTURE '+letter+' '+out.name;ready=out/(letter+'.json')
 child=subprocess.Popen([sys.executable,str(Path(__file__).resolve()),'--fixture',tag,str(ready),'fixture-value-张三'],creationflags=subprocess.CREATE_NO_WINDOW)
 children.append(child);wait(ready.exists);info=json.loads(ready.read_text(encoding='utf-8'));child.fixture_pid=info['pid'];return child,tag,info

def set_search(value):
 buffer=c.create_unicode_buffer(value);p.u.SendMessageW(ctl(1101),12,0,c.addressof(buffer))

def focus(child,tag):
 subprocess.run([str(exe),'--focus-pid',str(child.fixture_pid)],check=True,timeout=5)
 set_search(tag)
 wait(lambda:str(child.fixture_pid) in txt(1108) and tag in txt(1101))

def confirm(child,approve):
 wait(lambda:p.u.IsWindowEnabled(ctl(1114)))
 p.u.PostMessageW(ctl(1114),0xf5,0,0)
 def dialog():
  found=[]
  cb=c.WINFUNCTYPE(w.BOOL,w.HWND,w.LPARAM)(lambda h,_:(found.append(h) if p.pid(h)==tool.pid and p.u.GetDlgItem(h,1) and p.u.GetDlgItem(h,2) and 'End process' in p.text(h) else None) or True)
  p.u.EnumWindows(cb,0);return found[0] if found else None
 dlg=wait(dialog)
 texts=[];cb=c.WINFUNCTYPE(w.BOOL,w.HWND,w.LPARAM)(lambda h,_:(texts.append(p.text(h)) or True));p.u.EnumChildWindows(dlg,cb,0)
 assert str(child.fixture_pid) in '\n'.join(texts),'Refusing to confirm an unrelated target'
 default=p.u.SendMessageW(dlg,0x0400,0,0)&0xffff;assert default==2,'Cancellation must be default'
 p.u.SendMessageW(p.u.GetDlgItem(dlg,1 if approve else 2),0xf5,0,0)
 wait(lambda:not p.u.IsWindow(dlg))

try:
 if args.section=='all':
  a,tag,a_info=fixture('A')
 else:
  a,tag,a_info=fixture('B')
 tool=subprocess.Popen([str(exe),'--language','en','--theme','dark','--focus-pid',str(a.fixture_pid)],creationflags=subprocess.CREATE_NO_WINDOW)
 hwnd=wait(lambda:main_window(tool.pid))
 wait(lambda:str(a.fixture_pid) in txt(1108) and 'fixture-value-张三' in txt(1108),timeout=18)
 set_search(tag);wait(lambda:p.u.SendMessageW(ctl(1107),0x1004,0,0)==1)
 assert p.u.IsWindowEnabled(ctl(1111)),'Command line must be copyable after WMI completes'
 p.capture_settings(out/'tasks-en-dark.png',hwnd)
 confirm(a,False);assert a.poll() is None
 if args.section=='all':
  click(1103);wait(lambda:p.u.SendMessageW(ctl(1107),0x1004,0,0)>=2)
  assert str(a.fixture_pid) in txt(1108)
  p.capture_settings(out/'network-en-dark.png',hwnd)
  click(1103);wait(lambda:p.u.SendMessageW(ctl(1107),0x1004,0,0)==1)
  p.u.PostMessageW(a_info["hwnd"],0x800a,0,0);wait((out/'A.blocked').exists);click(1112)
  wait(lambda:'not responding' in txt(1115),timeout=1.0)
  response=c.c_size_t();assert p.u.SendMessageTimeoutW(hwnd,14,0,0,0x22,400,c.byref(response)),'UI must remain responsive while target is hung'
  wait((out/'A.resumed').exists)
  p.u.ShowWindow(a_info["hwnd"],6);click(1112);wait(lambda:not p.u.IsIconic(a_info["hwnd"]) and p.u.GetForegroundWindow()==a_info["hwnd"])
  focus(a,tag)
  click(1113);assert a.wait(timeout=5)==0
  b,tag,b_info=fixture('B');focus(b,tag)
  wait(lambda:'fixture-value-张三' in txt(1108),timeout=18)
 else:
  b=a
 confirm(b,True);assert b.wait(timeout=5)==1
 wait(lambda:'Process ended' in txt(1115));wait(lambda:p.u.SendMessageW(ctl(1107),0x1004,0,0)==0)
 click(1105);click(1106)
 wait(lambda:'我的任务' in p.text(hwnd) and '进程已结束' in txt(1115))
 p.capture_settings(out/'tasks-zh-light.png',hwnd)
 report.update(passed=True,section=args.section,real_wmi_command_line=True,cancel_preserved_target=True,explicit_termination=True,two_representative_styles=True)
 if args.section=='all':report.update(search_and_network=True,hung_target_feedback_and_responsive_ui=True,window_activation=True,normal_close=True)
except BaseException as error:
 report.update(error=repr(error),status=txt(1115) if hwnd else None)
 raise
finally:
 if tool and tool.poll() is None:
  if hwnd:p.u.PostMessageW(hwnd,16,0,0)
  try:
   if tool.wait(timeout=8)!=0:report['passed']=False
  except subprocess.TimeoutExpired:tool.kill();tool.wait();report['forced_owned_tool_cleanup']=True;report['passed']=False
 for child in children:
  if child.poll() is None:
   row=next((json.loads(f.read_text(encoding='utf-8')) for f in [out/'A.json',out/'B.json'] if f.exists() and json.loads(f.read_text(encoding='utf-8'))['pid']==getattr(child,'fixture_pid',child.pid)),None)
   if row:p.u.PostMessageW(row['hwnd'],16,0,0)
   try:child.wait(timeout=3)
   except subprocess.TimeoutExpired:child.kill();child.wait()
 if original and p.u.IsWindow(original):p.u.SetForegroundWindow(original)
 report['original_winmgr_pid_shell_and_workarea_unchanged']=p.pid(p.message())==original_winmgr and p.shell_settings()==shell and desktop.workarea()==area
 report['passed']=report['passed'] and report['original_winmgr_pid_shell_and_workarea_unchanged']
 (out/'summary.json').write_text(json.dumps(report,indent=2),encoding='utf-8');print(json.dumps(report),flush=True)
if not report['passed']:raise RuntimeError('Task-tool smoke failed; see private report')
