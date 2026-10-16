"""Native input and owned Run-window observation; private test output only."""
OUTPUT=None
import ctypes as c
from ctypes import wintypes as w
import json, time, sys, pathlib
u=c.WinDLL('user32',use_last_error=True)
k=c.WinDLL('kernel32',use_last_error=True)
u.SetProcessDPIAware()
u.FindWindowW.argtypes=[w.LPCWSTR,w.LPCWSTR]; u.FindWindowW.restype=w.HWND
u.GetForegroundWindow.restype=w.HWND
u.GetDlgItem.argtypes=[w.HWND,c.c_int];u.GetDlgItem.restype=w.HWND
u.GetWindowTextLengthW.argtypes=[w.HWND]
u.GetWindowTextW.argtypes=[w.HWND,w.LPWSTR,c.c_int]
u.SendMessageW.argtypes=[w.HWND,w.UINT,w.WPARAM,w.LPARAM];u.SendMessageW.restype=w.LPARAM
u.IsWindowVisible.argtypes=[w.HWND];u.IsWindowEnabled.argtypes=[w.HWND]
u.GetWindowRect.argtypes=[w.HWND,c.POINTER(w.RECT)]
u.GetGUIThreadInfo.argtypes=[w.DWORD,c.c_void_p]
u.GetWindowThreadProcessId.argtypes=[w.HWND,c.POINTER(w.DWORD)]
u.GetGuiResources.argtypes=[w.HANDLE,w.DWORD];u.GetGuiResources.restype=w.DWORD
k.OpenProcess.argtypes=[w.DWORD,w.BOOL,w.DWORD];k.OpenProcess.restype=w.HANDLE
k.CloseHandle.argtypes=[w.HANDLE]
class KEY(c.Structure):_fields_=[('vk',w.WORD),('scan',w.WORD),('flags',w.DWORD),('time',w.DWORD),('extra',c.c_size_t)]
class MOUSE(c.Structure):_fields_=[('dx',w.LONG),('dy',w.LONG),('data',w.DWORD),('flags',w.DWORD),('time',w.DWORD),('extra',c.c_size_t)]
class UNION(c.Union):_fields_=[('key',KEY),('mouse',MOUSE)]
class INPUT(c.Structure):_fields_=[('kind',w.DWORD),('value',UNION)]
class GTI(c.Structure):_fields_=[('size',w.DWORD),('flags',w.DWORD),('active',w.HWND),('focus',w.HWND),('capture',w.HWND),('menu',w.HWND),('move',w.HWND),('caret',w.HWND),('rect',w.RECT)]
u.SendInput.argtypes=[w.UINT,c.POINTER(INPUT),c.c_int]
VK={'WIN':0x5b,'CTRL':0x11,'ALT':0x12,'SHIFT':0x10,'ENTER':0x0d,'ESC':0x1b,'TAB':9,'UP':0x26,'DOWN':0x28,'SPACE':0x20,'F4':0x73,'BACK':8,'HOME':0x24,'END':0x23}
def send(items):
    arr=(INPUT*len(items))(*items)
    if u.SendInput(len(items),arr,c.sizeof(INPUT))!=len(items):raise OSError(c.get_last_error(),'SendInput')
def key(name):
    codes=[VK.get(n.upper(),ord(n.upper()) if len(n)==1 else 0) for n in name.split('+')]
    send([INPUT(1,UNION(key=KEY(v,0,0,0,0))) for v in codes]+[INPUT(1,UNION(key=KEY(v,0,2,0,0))) for v in reversed(codes)])
def typing(text):
    for unit in [int.from_bytes(text.encode('utf-16le')[i:i+2],'little') for i in range(0,len(text.encode('utf-16le')),2)]:
        send([INPUT(1,UNION(key=KEY(0,unit,4,0,0))),INPUT(1,UNION(key=KEY(0,unit,6,0,0)))])
def popup():return u.FindWindowW('minicon_winmgr_run_dialog',None)
def get_text(hwnd):
    # GetWindowText cannot retrieve another process's EDIT value. WM_GETTEXT
    # is a system-marshalled message and reads the actual control content.
    b=c.create_unicode_buffer(u.SendMessageW(hwnd,0x000e,0,0)+1);u.SendMessageW(hwnd,0x000d,len(b),c.addressof(b));return b.value
def state():
    h=popup(); pid=w.DWORD();tid=u.GetWindowThreadProcessId(h,c.byref(pid));g=GTI();g.size=c.sizeof(g);u.GetGUIThreadInfo(tid,c.byref(g));r=w.RECT();u.GetWindowRect(h,c.byref(r))
    out={'hwnd':h,'pid':pid.value,'visible':bool(u.IsWindowVisible(h)),'foreground':u.GetForegroundWindow(),'focus':g.focus,'rect':[r.left,r.top,r.right,r.bottom],'controls':{}}
    for ident in range(2001,2010):
        child=u.GetDlgItem(h,ident)
        if not child:continue
        rect=w.RECT();u.GetWindowRect(child,c.byref(rect))
        item={'hwnd':child,'text':get_text(child),'visible':bool(u.IsWindowVisible(child)),'enabled':bool(u.IsWindowEnabled(child)),'rect':[rect.left,rect.top,rect.right,rect.bottom]}
        if ident==2006:
            n=u.SendMessageW(child,0x018b,0,0);item['selection']=u.SendMessageW(child,0x0188,0,0);item['items']=[]
            for i in range(max(0,min(n,20))):
                b=c.create_unicode_buffer(u.SendMessageW(child,0x018a,i,0)+1);u.SendMessageW(child,0x0189,i,c.addressof(b));item['items'].append(b.value)
        out['controls'][str(ident)]=item
    process=k.OpenProcess(0x1000,False,pid.value)
    if process:
        out['gdi']=u.GetGuiResources(process,0);out['user']=u.GetGuiResources(process,1);k.CloseHandle(process)
    return out
def click_control(ident):
    h=u.GetDlgItem(popup(),ident);r=w.RECT();u.GetWindowRect(h,c.byref(r));
    if not u.SetCursorPos((r.left+r.right)//2,(r.top+r.bottom)//2):raise RuntimeError("Input desktop refused cursor movement")
    send([INPUT(0,UNION(mouse=MOUSE(0,0,0,2,0,0))),INPUT(0,UNION(mouse=MOUSE(0,0,0,4,0,0)))])
def capture(name):
    import settings_probe as p
    time.sleep(.12)
    path=OUTPUT/'captures'/name;path.parent.mkdir(exist_ok=True)
    p.capture_settings(path,popup())
    return str(path)
