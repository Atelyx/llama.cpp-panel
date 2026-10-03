/**
 * 自动启动待命脚本（PowerShell / Python / Perl 三份同语义实现，watcher 写盘后经解释器执行）。
 * 内容保持纯 ASCII：PowerShell 5.1 对无 BOM 文件按 ANSI 读，非 ASCII 注释会变乱码。
 *
 * 触发契约（watcher 据此编排）：
 * - 待命：占住端口循环 accept；无数据连接（端口扫描）与首页 GET / 不触发；
 * - 触发：收到请求即向 stdout 打印请求首行（watcher 的启动信号），随即让出端口并保持该连接，
 *   等 llama-server 就绪（/health 不再返回 503，最长 3 分钟）后转发请求、双向回传，完成后自灭；
 * - 触发路径退出码恒为 0（watcher 以 stdout 首行为准），其余退出 = 待命异常。
 */

const PS_SCRIPT = String.raw`param(
  [string]$BindHost = "127.0.0.1",
  [int]$Port = 8080,
  [string]$ServerHost = "127.0.0.1"
)
$ErrorActionPreference = "Stop"

function Test-ServerReady {
  try {
    $p = [System.Net.Sockets.TcpClient]::new()
    if (-not $p.ConnectAsync($ServerHost, $Port).Wait(500)) { $p.Close(); return $false }
    $s = $p.GetStream()
    $s.ReadTimeout = 500
    $s.WriteTimeout = 500
    $req = [System.Text.Encoding]::ASCII.GetBytes("GET /health HTTP/1.0" + [char]13 + [char]10 + [char]13 + [char]10)
    $s.Write($req, 0, $req.Length)
    $buf = New-Object byte[] 64
    $n = $s.Read($buf, 0, 64)
    $status = [System.Text.Encoding]::ASCII.GetString($buf, 0, $n)
    $p.Close()
    return $status -notmatch ' 503 '
  } catch { return $false }
}

$listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Parse($BindHost), $Port)
$listener.Start()
try {
  while ($true) {
    $client = $listener.AcceptTcpClient()
    try {
      $cs = $client.GetStream()
      $cs.ReadTimeout = 2000
      $chunk = New-Object byte[] 65536
      try { $read = $cs.Read($chunk, 0, $chunk.Length) } catch { continue }
      if ($read -le 0) { continue }
      $request = [System.Text.Encoding]::ASCII.GetString($chunk, 0, $read)
      $firstLine = ($request -split '\r?\n')[0].Trim()
      $parts = $firstLine -split '\s+'
      $method = ""
      $path = ""
      if ($parts.Length -gt 0) { $method = $parts[0] }
      if ($parts.Length -gt 1) { $path = ($parts[1] -split '\?')[0] }
      if ($method -ieq "GET" -and $path -eq "/") { continue }
      Write-Output $firstLine
      $listener.Stop()

      # hold the connection and keep buffering the client request while waiting
      $ms = New-Object System.IO.MemoryStream
      $ms.Write($chunk, 0, $read)
      $cs.ReadTimeout = 250
      $deadline = [DateTime]::UtcNow.AddSeconds(180)
      $ready = $false
      while ([DateTime]::UtcNow -lt $deadline) {
        try {
          $n = $cs.Read($chunk, 0, $chunk.Length)
          if ($n -le 0) { exit 0 }
          $ms.Write($chunk, 0, $n)
        } catch { }
        if (Test-ServerReady) { $ready = $true; break }
      }
      if (-not $ready) { exit 0 }

      # blind byte relay both directions: no HTTP parsing, streaming (SSE) passes through
      $server = [System.Net.Sockets.TcpClient]::new()
      if (-not $server.ConnectAsync($ServerHost, $Port).Wait(5000)) { exit 0 }
      $ss = $server.GetStream()
      $ss.WriteTimeout = 10000
      $sb = $ms.ToArray()
      $ss.Write($sb, 0, $sb.Length)
      $last = [DateTime]::UtcNow
      while ($true) {
        $moved = $false
        try {
          if ($cs.DataAvailable) {
            $n = $cs.Read($chunk, 0, $chunk.Length)
            if ($n -le 0) { break }
            $ss.Write($chunk, 0, $n)
            $last = [DateTime]::UtcNow
            $moved = $true
          }
        } catch { break }
        try {
          if ($ss.DataAvailable) {
            $n = $ss.Read($chunk, 0, $chunk.Length)
            if ($n -le 0) { break }
            $cs.Write($chunk, 0, $n)
            $last = [DateTime]::UtcNow
            $moved = $true
          }
        } catch { break }
        if (-not $moved) { Start-Sleep -Milliseconds 10 }
        if (([DateTime]::UtcNow - $last).TotalSeconds -gt 300) { break }
      }
      exit 0
    } finally { $client.Close() }
  }
} finally {
  try { $listener.Stop() } catch { }
}`;

const PY_SCRIPT = String.raw`import socket
import sys
import threading
import time

bind_host, port, server_host = sys.argv[1], int(sys.argv[2]), sys.argv[3]
WAIT_SECONDS = 180
IDLE_SECONDS = 300

def server_ready():
    try:
        s = socket.create_connection((server_host, port), timeout=0.5)
        s.sendall(b"GET /health HTTP/1.0\r\n\r\n")
        status = s.recv(64).decode("latin-1", "replace")
        s.close()
        return " 503 " not in status
    except OSError:
        return False

srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind((bind_host, port))
srv.listen(4)
while True:
    conn, _ = srv.accept()
    try:
        conn.settimeout(2)
        try:
            data = conn.recv(65536)
        except OSError:
            continue
        if not data:
            continue
        first = data.split(b"\r\n", 1)[0].decode("latin-1", "replace").strip()
        parts = first.split()
        method = parts[0].upper() if parts else ""
        path = parts[1].split("?", 1)[0] if len(parts) > 1 else ""
        if method == "GET" and path == "/":
            continue
        print(first, flush=True)
        srv.close()  # yield the port; keep the client for forwarding
        buf = bytearray(data)
        conn.settimeout(0.25)
        deadline = time.time() + WAIT_SECONDS
        ready = False
        next_probe = 0.0
        while time.time() < deadline:
            try:
                chunk = conn.recv(65536)
                if not chunk:
                    sys.exit(0)
                buf += chunk
            except OSError:
                pass
            now = time.time()
            if now >= next_probe:
                next_probe = now + 0.5
                if server_ready():
                    ready = True
                    break
        if not ready:
            conn.close()
            sys.exit(0)
        try:
            up = socket.create_connection((server_host, port), timeout=5)
        except OSError:
            conn.close()
            sys.exit(0)
        up.sendall(bytes(buf))
        conn.settimeout(IDLE_SECONDS)
        up.settimeout(IDLE_SECONDS)
        def pump(a, b):
            try:
                while True:
                    d = a.recv(65536)
                    if not d:
                        break
                    b.sendall(d)
            except OSError:
                pass
            try:
                b.shutdown(socket.SHUT_WR)
            except OSError:
                pass
        threading.Thread(target=pump, args=(conn, up), daemon=True).start()
        try:
            while True:
                d = up.recv(65536)
                if not d:
                    break
                conn.sendall(d)
        except OSError:
            pass
        sys.exit(0)
    finally:
        conn.close()`;

const PL_SCRIPT = String.raw`use strict;
use warnings;
use IO::Socket::INET;
use IO::Select;

$SIG{PIPE} = "IGNORE";
my ($bind_host, $port, $server_host) = @ARGV;
my $srv = IO::Socket::INET->new(
  LocalAddr => $bind_host, LocalPort => $port, Proto => "tcp", Listen => 5, ReuseAddr => 1,
) or die "bind $bind_host:$port failed: $!";

sub server_ready {
  my $s = IO::Socket::INET->new(PeerAddr => $server_host, PeerPort => $port, Proto => "tcp", Timeout => 0.5);
  return 0 unless $s;
  print $s "GET /health HTTP/1.0\r\n\r\n";
  my $status = "";
  $s->recv($status, 64);
  $s->close;
  return $status !~ / 503 / ? 1 : 0;
}

sub flush_write {
  my ($socket, $bytes) = @_;
  my $off = 0;
  while ($off < length($bytes)) {
    my $n = syswrite($socket, $bytes, length($bytes) - $off, $off);
    return 0 unless defined $n;
    $off += $n;
  }
  return 1;
}

while (1) {
  my $conn = $srv->accept or die "accept failed: $!";
  $conn->blocking(0);
  my $data = "";
  {
    my $sel0 = IO::Select->new($conn);
    if ($sel0->can_read(2)) {
      my $n = sysread($conn, $data, 65536);
      $data = "" unless defined $n && $n > 0;
    }
  }
  if ($data eq "") { close $conn; next; }
  (my $first = $data) =~ s/\r?\n.*//s;
  $first =~ s/\s+$//;
  my @parts = split /\s+/, $first;
  my $method = uc($parts[0] // "");
  (my $path = $parts[1] // "") =~ s/\?.*//;
  if ($method eq "GET" && $path eq "/") { close $conn; next; }
  print "$first\n";
  close $srv;    # yield the port; keep the client for forwarding
  undef $srv;
  my $buf = $data;
  my $deadline = time() + 180;
  my $ready = 0;
  my $next_probe = 0;
  my $selc = IO::Select->new($conn);
  while (time() < $deadline) {
    if ($selc->can_read(0.25)) {
      my $chunk;
      my $n = sysread($conn, $chunk, 65536);
      exit 0 if defined $n && $n == 0;    # client gave up
      $buf .= $chunk if $n;
    }
    if (time() >= $next_probe) {
      $next_probe = time() + 0.5;
      if (server_ready()) { $ready = 1; last; }
    }
  }
  exit 0 unless $ready;
  my $up = IO::Socket::INET->new(PeerAddr => $server_host, PeerPort => $port, Proto => "tcp", Timeout => 5) or exit 0;
  $up->blocking(0);
  exit 0 unless flush_write($up, $buf);
  my $sel = IO::Select->new($conn, $up);
  my $last = time();
  while (1) {
    my @readable = $sel->can_read(0.25);
    for my $sock (@readable) {
      my $chunk;
      my $n = sysread($sock, $chunk, 65536);
      if (!defined $n || $n == 0) { $sel->remove($sock); next; }
      my $dst = $sock == $conn ? $up : $conn;
      exit 0 unless flush_write($dst, $chunk);
      $last = time();
    }
    exit 0 if $sel->count == 0;
    exit 0 if time() - $last > 300;
  }
}`;

export { PS_SCRIPT, PY_SCRIPT, PL_SCRIPT };
