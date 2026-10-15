TV Controller server for Windows
================================

The folder layout below is what the server expects. Keep it together, for
example as C:\tv-controller:

  server.exe        the server
  .env              settings (edit before first start)
  start.bat         run by hand, in a window
  dashboard\        the web dashboard
  videos\           put your videos and images here (flat, no subfolders)

The database (tv-controller.db) and data\thumbnails\ are created on first run.


1. Set the server address
-------------------------
Run `ipconfig` and note this PC's IPv4 address on the network the Pis use.
Give the PC a fixed address (a DHCP reservation on the router, or a static IP),
or a reboot can move it and every Pi loses the server.

Open .env in Notepad, remove the # in front of SERVER_BASE_URL and set it:

  SERVER_BASE_URL=http://192.168.1.50:8000

The server will not start until this is set.


2. Optional: ffmpeg, for durations and thumbnails
-------------------------------------------------
Without it everything still plays; the library just shows no running time and
no poster images. Download a Windows build (https://www.gyan.dev/ffmpeg/builds/,
"release essentials"), and copy ffmpeg.exe and ffprobe.exe from its bin\
folder into this folder, next to server.exe. Putting them here, rather than on
PATH, means the scheduled task below finds them too.


3. Allow the Pis through the firewall
-------------------------------------
In PowerShell, run as administrator:

  New-NetFirewallRule -DisplayName "TV Controller" -Direction Inbound -Protocol TCP -LocalPort 8000 -Action Allow

Also set the network to Private: Settings > Network & internet > (your
connection) > Network profile type.


4. Try it
---------
Double-click start.bat. Open http://localhost:8000 on this PC, then
http://<this PC's IP>:8000 from a phone on the same network. Close the window
(or press Ctrl+C) to stop it.

On each Pi, set SERVER_URL=http://<this PC's IP>:8000 in /etc/tv-agent/.env and
run `sudo systemctl restart tv-agent`. Its tile appears within seconds.


5. Run it at boot, unattended
-----------------------------
This starts the server when Windows starts, with nobody signed in, restarts it
if it exits, and writes its output to server.log. In PowerShell as
administrator, from this folder:

  $dir = (Get-Location).Path
  $action = New-ScheduledTaskAction -Execute "cmd.exe" -Argument "/c server.exe >> server.log 2>&1" -WorkingDirectory $dir
  $trigger = New-ScheduledTaskTrigger -AtStartup
  $settings = New-ScheduledTaskSettingsSet -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
  Register-ScheduledTask -TaskName "TV Controller" -Action $action -Trigger $trigger -Settings $settings -User "SYSTEM" -RunLevel Highest
  Start-ScheduledTask -TaskName "TV Controller"

Stop it with   Stop-ScheduledTask -TaskName "TV Controller"
Remove it with Unregister-ScheduledTask -TaskName "TV Controller"

Do not run start.bat while the task is running: both want port 8000.

Also set Settings > System > Power > Sleep to Never. A sleeping PC takes every
TV's stream with it.


Adding media
------------
Copy files into videos\. They are picked up automatically. Encode videos for
the weakest Pi in the fleet: see scripts/transcode.sh in the repository.


Troubleshooting
---------------
- Window closes immediately / task keeps restarting: read server.log (or run
  start.bat to see the error). Most often SERVER_BASE_URL is not set, or port
  8000 is already in use.
- Dashboard works here but not from a phone: firewall rule or Public network
  profile (step 3).
- TV tile appears but nothing plays: SERVER_BASE_URL is wrong; the Pi must be
  be able to open that address itself (try curl from the Pi).
