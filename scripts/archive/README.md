# 归档脚本

这些探针已没有测试层、工作流或其他脚本引用。保留源码只为追溯，运行前需要按当前接口修改。

- `notification_probe.py`：通知宿主的早期压力探针（0.1.11 之前的通知路径）。
- `legacy_update_path_probe.py`：0.1.14 旧 helper 改名协议的探针，对应的修复见 [docs/archive/update_filename_fix.md](../../docs/archive/update_filename_fix.md)。

当前使用的探针见 [docs/testing.md](../../docs/testing.md)。
