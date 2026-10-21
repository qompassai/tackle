// Tackle editor bridge: connects out to the tackle server, presents the
// per-run capability token, and runs each command on the main thread.
//
// Contract: the socket task only moves bytes. Every command is queued and
// executed by EditorApplication.update on the main thread, one per tick, in
// arrival order; the socket task does not read the next command until the
// current reply is sent. Messages above MessageBytesMax close the socket.

using System;
using System.Collections.Concurrent;
using System.IO;
using System.Net.WebSockets;
using System.Text;
using System.Text.RegularExpressions;
using System.Threading;
using System.Threading.Tasks;
using UnityEditor;
using UnityEngine;

namespace Tackle.Editor
{
    [InitializeOnLoad]
    static class TackleBridge
    {
        const int MessageBytesMax = 1024 * 1024;
        const int CapabilityFileBytesMax = 4096;
        const int ReceiveBufferBytes = 8192;
        const int ReconnectDelayMs = 2000;
        const string LogPrefix = "[Tackle] ";

        sealed class Command
        {
            public string Text;
            public TaskCompletionSource<string> Reply;
        }

        static readonly ConcurrentQueue<Command> s_Inbox = new ConcurrentQueue<Command>();
        static readonly Regex s_PortField = new Regex("\"port\"\\s*:\\s*(\\d{1,5})");
        static readonly Regex s_TokenField = new Regex("\"token\"\\s*:\\s*\"([0-9a-f]{64})\"");
        static CancellationTokenSource s_Cancel;
        static ClientWebSocket s_Socket;

        internal static int MainThreadId { get; private set; }
        internal static int QueueDepth => s_Inbox.Count;

        // InitializeOnLoad runs this on the main thread after every domain load.
        static TackleBridge()
        {
            MainThreadId = Thread.CurrentThread.ManagedThreadId;
            // Import workers are separate Editor processes that load editor
            // assemblies too; observed in U0 stealing the session and never
            // pumping. Only the interactive Editor connects.
            if (AssetDatabase.IsAssetImportWorkerProcess())
                return;
            EditorApplication.update += Pump;
            AssemblyReloadEvents.beforeAssemblyReload += Stop;
            EditorApplication.quitting += Stop;
            s_Cancel = new CancellationTokenSource();
            var cancel = s_Cancel.Token;
            Task.Run(() => RunAsync(cancel));
        }

        static void Stop()
        {
            s_Cancel?.Cancel();
            // Synchronous: the domain may unload before a graceful close runs.
            s_Socket?.Abort();
        }

        static void Pump()
        {
            if (!s_Inbox.TryDequeue(out var command))
                return;
            command.Reply.TrySetResult(TackleCommands.Execute(command.Text));
        }

        static async Task RunAsync(CancellationToken cancel)
        {
            string lastFailure = null;
            while (!cancel.IsCancellationRequested)
            {
                try
                {
                    var (port, token) = ReadCapability();
                    using (var socket = new ClientWebSocket())
                    {
                        s_Socket = socket;
                        socket.Options.SetRequestHeader("Authorization", "Bearer " + token);
                        await socket.ConnectAsync(new Uri($"ws://127.0.0.1:{port}/editor"), cancel);
                        Debug.Log(LogPrefix + $"session connected to 127.0.0.1:{port} via System.Net.WebSockets.ClientWebSocket");
                        lastFailure = null;
                        await ServeAsync(socket, cancel);
                        Debug.Log(LogPrefix + "session closed");
                    }
                }
                catch (OperationCanceledException) when (cancel.IsCancellationRequested)
                {
                    break;
                }
                catch (Exception error)
                {
                    var failure = error.GetType().Name + ": " + error.Message;
                    if (failure != lastFailure)
                        Debug.Log(LogPrefix + "not connected (" + failure + "); retrying every " + ReconnectDelayMs + " ms");
                    lastFailure = failure;
                }
                try
                {
                    await Task.Delay(ReconnectDelayMs, cancel);
                }
                catch (OperationCanceledException)
                {
                    break;
                }
            }
        }

        static async Task ServeAsync(ClientWebSocket socket, CancellationToken cancel)
        {
            var buffer = new byte[ReceiveBufferBytes];
            var message = new MemoryStream();
            while (socket.State == WebSocketState.Open)
            {
                message.SetLength(0);
                WebSocketReceiveResult received;
                do
                {
                    received = await socket.ReceiveAsync(new ArraySegment<byte>(buffer), cancel);
                    if (received.MessageType == WebSocketMessageType.Close)
                        return;
                    if (message.Length + received.Count > MessageBytesMax)
                    {
                        await socket.CloseOutputAsync(WebSocketCloseStatus.MessageTooBig, "message too big", cancel);
                        return;
                    }
                    message.Write(buffer, 0, received.Count);
                } while (!received.EndOfMessage);
                if (received.MessageType != WebSocketMessageType.Text)
                    continue;
                var command = new Command
                {
                    Text = Encoding.UTF8.GetString(message.GetBuffer(), 0, (int)message.Length),
                    Reply = new TaskCompletionSource<string>(TaskCreationOptions.RunContinuationsAsynchronously),
                };
                string reply;
                using (cancel.Register(() => command.Reply.TrySetCanceled()))
                {
                    s_Inbox.Enqueue(command);
                    reply = await command.Reply.Task;
                }
                var bytes = Encoding.UTF8.GetBytes(reply);
                await socket.SendAsync(new ArraySegment<byte>(bytes), WebSocketMessageType.Text, true, cancel);
            }
        }

        // Same resolution order as the server: TACKLE_STATE_DIR, XDG_STATE_HOME, HOME.
        static (int, string) ReadCapability()
        {
            var path = Path.Combine(StateDir(), "capability", "editor.json");
            var info = new FileInfo(path);
            if (!info.Exists)
                throw new FileNotFoundException("no capability file at " + path);
            if (info.Length > CapabilityFileBytesMax)
                throw new InvalidDataException("capability file exceeds " + CapabilityFileBytesMax + " bytes");
            var text = File.ReadAllText(path);
            var port = s_PortField.Match(text);
            var token = s_TokenField.Match(text);
            if (!port.Success || !token.Success)
                throw new InvalidDataException("capability file lacks port or token");
            var portNumber = int.Parse(port.Groups[1].Value);
            if (portNumber < 1024 || portNumber > 65535)
                throw new InvalidDataException("capability port out of range");
            return (portNumber, token.Groups[1].Value);
        }

        static string StateDir()
        {
            var explicitDir = Environment.GetEnvironmentVariable("TACKLE_STATE_DIR");
            if (!string.IsNullOrEmpty(explicitDir))
                return explicitDir;
            var xdg = Environment.GetEnvironmentVariable("XDG_STATE_HOME");
            if (!string.IsNullOrEmpty(xdg))
                return Path.Combine(xdg, "tackle");
            var home = Environment.GetEnvironmentVariable("HOME");
            if (string.IsNullOrEmpty(home))
                throw new InvalidOperationException("no TACKLE_STATE_DIR, XDG_STATE_HOME or HOME");
            return Path.Combine(home, ".local", "state", "tackle");
        }
    }
}
