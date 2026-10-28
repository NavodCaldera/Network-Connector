import time, winreg
from winrt.windows.ui.notifications import ToastNotificationManager, ToastNotification, NotificationSetting
from winrt.windows.data.xml.dom import XmlDocument
A = "NetworkConnector.SpikeClean"
for k in (rf"Software\Classes\AppUserModelId\{A}", rf"Software\Microsoft\Windows\CurrentVersion\Notifications\Settings\{A}"):
    try:
        winreg.OpenKey(winreg.HKEY_CURRENT_USER, k); print("pre-existing:", k)
    except OSError:
        print("absent before run:", k)
n = ToastNotificationManager.create_toast_notifier_with_id(A)
try: print("setting:", NotificationSetting(n.setting).name)
except Exception as e: print("setting failed:", e)
d = XmlDocument(); d.load_xml('<toast><visual><binding template="ToastGeneric"><text>Network Connector</text><text>Clean unregistered AUMID test</text></binding></visual></toast>')
t = ToastNotification(d); t.tag = "clean"
n.show(t); print("show returned"); time.sleep(3)
print("history items:", len(list(ToastNotificationManager.history.get_history_with_id(A))))
try: print("setting after show:", NotificationSetting(n.setting).name)
except Exception as e: print("setting after show failed:", e)
ToastNotificationManager.history.clear_with_id(A)
