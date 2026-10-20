"""Focused Winmgr -> task-tool entry acceptance; no UAC or registry writes."""
import argparse,ctypes as c,json,os,shutil,subprocess,time
from ctypes import wintypes as w
from pathlib import Path
import desktop_session as ds,memory_probe as m,scenario_probe as s,settings_probe as p

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--main',type=Path,required=True)
parser.add_argument('--tool',type=Path,required=True)
parser.add_argument('--restore-exe',type=Path,required=True)
args=parser.parse_args()
main=args.main.resolve(strict=True);tool=args.tool.resolve(strict=True)
# Derive names/version from the candidate, never from the working tree.
version=tool.name.removeprefix('minicon-tskmgr-v').removesuffix('-x86_64-windows.exe')
assert version!='' and tool.name==f'minicon-tskmgr-v{version}-x86_64-windows.exe'
out=s.ROOT/'target/test-reports/tool-entry'/str(time.time_ns());out.mkdir(parents=True)
report=dict(passed=False,main_sha256=s.digest(main),tool_sha256=s.digest(tool))
child=None;owned=[]
def windows():
 rows=[]
 cb=c.WINFUNCTYPE(w.BOOL,w.HWND,w.LPARAM)(lambda h,_:(rows.append(h) if p.u.IsWindowVisible(h) else None) or True)
 p.u.EnumWindows(cb,0);return rows

def find_dialog():
 return next((h for h in windows() if p.pid(h)==child.pid and p.text(h)=='Minicon Tskmgr' and p.u.GetDlgItem(h,6) and p.u.GetDlgItem(h,7)),None)

def task_window():
 return next((h for h in windows() if p.text(h).startswith('minicon-tskmgr') and version in p.text(h)),None)

def close_tool(hwnd):
 process=m.Process(p.pid(hwnd))
 try:
  assert desktop.image(process).is_relative_to(p.LIVE.parent) or desktop.image(process).is_relative_to(out)
  p.u.PostMessageW(hwnd,16,0,0)
  assert p.k.WaitForSingleObject(process.handle,8000)==0
 finally:process.close()

assert not task_window(), 'Close an existing same-version task tool before this owned probe'
try:
 with ds.isolated(main,args.restore_exe) as isolation:
  desktop=s.Desktop()
  shutil.copy2(main,p.LIVE)
  config=out/'settings.conf';config.write_text('ui.language=en\nappearance.mode=dark\nbehavior.hover_preview=false\n',encoding='utf-8')
  env={k:v for k,v in os.environ.items() if not k.startswith('MINICON_')}
  env.update(MINICON_SETTINGS=str(config),MINICON_CONFIG=str(out/'empty.conf'),MINICON_LOG_DIR=str(out/'logs'),LOCALAPPDATA=str(out/'profile'))
  child=desktop.start(p.LIVE,env);p.show()
  p.focus_id(6410);p.key('ENTER');dlg=p.wait(find_dialog)
  texts=[];cb=c.WINFUNCTYPE(w.BOOL,w.HWND,w.LPARAM)(lambda h,_:(texts.append(p.text(h)) or True));p.u.EnumChildWindows(dlg,cb,0)
  assert version in '\n'.join(texts) and 'Download and open' in '\n'.join(texts)
  assert p.u.SendMessageW(dlg,0x400,0,0)&0xffff==7
  desktop.send(p.message(),0x8000+142,pid=child.pid)
  p.u.SendMessageW(p.u.GetDlgItem(dlg,7),0xf5,0,0)
  p.wait(lambda:'cancelled' in p.text(p.ctrl(6412)))
  assert not task_window()
  report['missing_tool_cancel_and_responsive_shell']=True
  sums=f'{s.digest(main)}  minicon-winmgr-v{version}-x86_64-windows.exe\n{s.digest(tool)}  {tool.name}\n'
  (p.LIVE.parent/'SHA256SUMS.txt').write_text(sums,encoding='ascii')
  sibling=p.LIVE.parent/tool.name;shutil.copy2(tool,sibling)
  p.click(6410);hwnd=p.wait(task_window);owned.append(hwnd)
  process=m.Process(p.pid(hwnd))
  try:assert desktop.image(process)==sibling.resolve()
  finally:process.close()
  assert child.poll() is None
  close_tool(hwnd);owned.remove(hwnd)
  assert child.poll() is None
  report['verified_sibling_entry_and_independent_exit']=True
  cache=out/'profile/minicon-winmgr/tools'/version;cache.mkdir(parents=True)
  shutil.move(sibling,cache/tool.name);(cache/'SHA256SUMS.txt').write_text(sums,encoding='ascii')
  p.click(6410);hwnd=p.wait(task_window);owned.append(hwnd)
  process=m.Process(p.pid(hwnd))
  try:assert desktop.image(process)==(cache/tool.name).resolve()
  finally:process.close()
  assert child.poll() is None
  close_tool(hwnd);owned.remove(hwnd)
  report['verified_versioned_cache_entry']=True
  p.capture_settings(out/'entry-en-dark.png',p.panel())
 report['original_desktop_restored']=bool(isolation['restored'])
 report['passed']=report['original_desktop_restored']
except BaseException as error:
 report.update(passed=False,error=repr(error))
 raise
finally:
 for hwnd in owned:
  if p.u.IsWindow(hwnd):p.u.PostMessageW(hwnd,16,0,0)
 (out/'summary.json').write_text(json.dumps(report,indent=2),encoding='utf-8')
 print(json.dumps(report),flush=True)
if not report['passed']:raise RuntimeError('Tool entry acceptance failed')
