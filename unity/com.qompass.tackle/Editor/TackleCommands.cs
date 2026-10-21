// Tackle commands. Main thread only: called from TackleBridge.Pump.
//
// Reply shape: {"id":N,"ok":true,"result":{...}} or {"id":N,"ok":false,"error":"..."}.
// Every command is read-only.

using System;
using System.Collections.Generic;
using System.IO;
using System.Text;
using System.Threading;
using UnityEditor;
using UnityEngine;
using UnityEngine.SceneManagement;

namespace Tackle.Editor
{
    static class TackleCommands
    {
        /// Levels of the hierarchy returned; roots are depth 0.
        internal const int SceneDepthMax = 4;
        /// Nodes returned before the walk stops.
        internal const int SceneNodeCountMax = 500;

        [Serializable]
        class Incoming
        {
            public long id;
            public string command;
        }

        internal static string Execute(string text)
        {
            Incoming incoming;
            try
            {
                incoming = JsonUtility.FromJson<Incoming>(text);
            }
            catch (Exception error)
            {
                return Failure(0, "malformed command: " + error.Message);
            }
            if (incoming == null || incoming.id <= 0 || string.IsNullOrEmpty(incoming.command))
                return Failure(0, "malformed command");
            try
            {
                switch (incoming.command)
                {
                    case "editor_state":
                        return Success(incoming.id, EditorState());
                    case "scene_hierarchy":
                        return Success(incoming.id, SceneHierarchy());
                    default:
                        return Failure(incoming.id, "unknown command `" + incoming.command + "`");
                }
            }
            catch (Exception error)
            {
                return Failure(incoming.id, error.GetType().Name + ": " + error.Message);
            }
        }

        static string EditorState()
        {
            var projectPath = Path.GetDirectoryName(Application.dataPath);
            var scene = SceneManager.GetActiveScene();
            var json = new StringBuilder("{");
            json.Append("\"unityVersion\":").Append(Quote(Application.unityVersion));
            json.Append(",\"projectName\":").Append(Quote(Path.GetFileName(projectPath)));
            json.Append(",\"productName\":").Append(Quote(PlayerSettings.productName));
            json.Append(",\"projectPath\":").Append(Quote(projectPath));
            json.Append(",\"activeScene\":{\"name\":").Append(Quote(scene.name));
            json.Append(",\"path\":").Append(Quote(scene.path)).Append('}');
            json.Append(",\"isPlaying\":").Append(Bool(EditorApplication.isPlaying));
            json.Append(",\"isCompiling\":").Append(Bool(EditorApplication.isCompiling));
            json.Append(",\"queueDepth\":").Append(TackleBridge.QueueDepth);
            json.Append(",\"onMainThread\":").Append(Bool(OnMainThread()));
            return json.Append('}').ToString();
        }

        // Preorder walk with an explicit stack: no recursion over scene depth.
        static string SceneHierarchy()
        {
            var scene = SceneManager.GetActiveScene();
            var roots = scene.GetRootGameObjects();
            var stack = new Stack<(Transform, int)>();
            for (var index = roots.Length - 1; index >= 0; index--)
                stack.Push((roots[index].transform, 0));
            var nodes = new StringBuilder();
            var nodeCount = 0;
            var truncatedByDepth = false;
            while (stack.Count > 0)
            {
                if (nodeCount == SceneNodeCountMax)
                    break;
                var (transform, depth) = stack.Pop();
                if (nodeCount > 0)
                    nodes.Append(',');
                nodes.Append("{\"name\":").Append(Quote(transform.name));
                nodes.Append(",\"active\":").Append(Bool(transform.gameObject.activeSelf));
                nodes.Append(",\"childCount\":").Append(transform.childCount);
                nodes.Append(",\"depth\":").Append(depth).Append('}');
                nodeCount++;
                if (transform.childCount == 0)
                    continue;
                if (depth + 1 >= SceneDepthMax)
                {
                    truncatedByDepth = true;
                    continue;
                }
                for (var child = transform.childCount - 1; child >= 0; child--)
                    stack.Push((transform.GetChild(child), depth + 1));
            }
            var json = new StringBuilder("{");
            json.Append("\"scene\":{\"name\":").Append(Quote(scene.name));
            json.Append(",\"path\":").Append(Quote(scene.path)).Append('}');
            json.Append(",\"rootCount\":").Append(roots.Length);
            json.Append(",\"depthMax\":").Append(SceneDepthMax);
            json.Append(",\"nodeCountMax\":").Append(SceneNodeCountMax);
            json.Append(",\"nodeCount\":").Append(nodeCount);
            json.Append(",\"truncatedByDepth\":").Append(Bool(truncatedByDepth));
            json.Append(",\"truncatedByCount\":").Append(Bool(stack.Count > 0));
            json.Append(",\"onMainThread\":").Append(Bool(OnMainThread()));
            json.Append(",\"nodes\":[").Append(nodes).Append("]");
            return json.Append('}').ToString();
        }

        static bool OnMainThread()
        {
            return Thread.CurrentThread.ManagedThreadId == TackleBridge.MainThreadId;
        }

        static string Success(long id, string resultJson)
        {
            return "{\"id\":" + id + ",\"ok\":true,\"result\":" + resultJson + "}";
        }

        static string Failure(long id, string message)
        {
            return "{\"id\":" + id + ",\"ok\":false,\"error\":" + Quote(message) + "}";
        }

        static string Bool(bool value)
        {
            return value ? "true" : "false";
        }

        static string Quote(string value)
        {
            var quoted = new StringBuilder("\"");
            foreach (var character in value ?? "")
            {
                switch (character)
                {
                    case '"': quoted.Append("\\\""); break;
                    case '\\': quoted.Append("\\\\"); break;
                    case '\n': quoted.Append("\\n"); break;
                    case '\r': quoted.Append("\\r"); break;
                    case '\t': quoted.Append("\\t"); break;
                    default:
                        if (character < 0x20)
                            quoted.Append("\\u").Append(((int)character).ToString("x4"));
                        else
                            quoted.Append(character);
                        break;
                }
            }
            return quoted.Append('"').ToString();
        }
    }
}
