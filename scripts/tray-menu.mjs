// 抓托盘右键菜单的截图。
//
// Windows 的托盘菜单不是普通窗口，PrintWindow 抓不到，只能真点真截。
// 流程：点任务栏溢出箭头 → UIA 在溢出面板里按 tooltip 找到我们的图标
// → 右键 → 等菜单画出来 → 全屏截图裁一块。
//
// 用法：node scripts/tray-menu.mjs [输出png] [工具提示前缀]

import { execFileSync } from 'node:child_process';

const out = process.argv[2] || 'D:\\Deepseek\\Alpha\\island-tray-menu.png';
const prefix = process.argv[3] || 'Lyric Island';

const ps = String.raw`
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Dpi { [DllImport("shcore.dll")] public static extern int SetProcessDpiAwareness(int v); }
public class M {
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, IntPtr e);
}
"@
[void][Dpi]::SetProcessDpiAwareness(2)
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms

function Click($x, $y, $right) {
  [void][M]::SetCursorPos($x, $y)
  Start-Sleep -Milliseconds 180
  [M]::mouse_event(0x0002, 0, 0, 0, [IntPtr]::Zero)
  Start-Sleep -Milliseconds 60
  [M]::mouse_event(0x0004, 0, 0, 0, [IntPtr]::Zero)
  if ($right) {
    Start-Sleep -Milliseconds 180
    [M]::mouse_event(0x0008, 0, 0, 0, [IntPtr]::Zero)
    Start-Sleep -Milliseconds 60
    [M]::mouse_event(0x0010, 0, 0, 0, [IntPtr]::Zero)
  }
}

# 1. 先把溢出面板打开：点任务栏上的 ^
$root = [System.Windows.Automation.AutomationElement]::RootElement
$btnCond = New-Object System.Windows.Automation.PropertyCondition(
  [System.Windows.Automation.AutomationElement]::NameProperty, '显示隐藏的图标')
$overflowBtn = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $btnCond)
if ($overflowBtn) {
  $b = $overflowBtn.Current.BoundingRectangle
  Click ([int]($b.X + $b.Width / 2)) ([int]($b.Y + $b.Height / 2)) $false
  Start-Sleep -Milliseconds 700
}

# 2. 在溢出面板里找我们的图标（元素名就是 tooltip）
$panel = $null
foreach ($cls in @('TopLevelWindowForOverflowXamlIsland', 'XamlExplorerHostIslandWindow')) {
  $c = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::ClassNameProperty, $cls)
  $panel = $root.FindFirst([System.Windows.Automation.TreeScope]::Children, $c)
  if ($panel) { break }
}

$target = $null
if ($panel) {
  $all = $panel.FindAll([System.Windows.Automation.TreeScope]::Descendants,
    [System.Windows.Automation.Condition]::TrueCondition)
  foreach ($e in $all) {
    if ($e.Current.Name -like "*${prefix}*") { $target = $e; break }
  }
}

if (-not $target) { Write-Output "NOT_FOUND"; exit 2 }

$r = $target.Current.BoundingRectangle
Write-Output ("FOUND " + $target.Current.Name + " @ " + [int]($r.X + $r.Width / 2) + "," + [int]($r.Y + $r.Height / 2))

Click ([int]($r.X + $r.Width / 2)) ([int]($r.Y + $r.Height / 2)) $true
Start-Sleep -Milliseconds 900

# 3. 截图：菜单一般出现在图标左侧，截一大块保险
$bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
$x = [Math]::Max(0, [int]($r.X - 700))
$y = [Math]::Max(0, [int]($r.Y - 620))
$w = [Math]::Min([int]($bounds.Width - $x), 900)
$h = [Math]::Min([int]($bounds.Height - $y), 800)

$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($x, $y, 0, 0, $bmp.Size)
$bmp.Save('${out}', [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Output ("SAVED " + $w + "x" + $h + " at " + $x + "," + $y)
`;

try {
  const res = execFileSync('powershell.exe', ['-NoProfile', '-Command', ps], { encoding: 'utf8' });
  console.log(res.trim());
} catch (e) {
  console.error('failed:', e.stdout || e.message);
  process.exit(1);
}
