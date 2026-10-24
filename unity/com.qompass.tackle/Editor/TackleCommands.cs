// Tackle commands. Main thread only: called from TackleBridge.Pump.
//
// Reply shape: {"id":N,"ok":true,"result":{...}} or {"id":N,"ok":false,"error":"..."}.
// Every command is read-only. U1 commands: editor_state, scene_hierarchy
// (paginated), gameobject_properties, console, project_info, selection.
// Frames may carry an "args" object; absent numeric args read as 0, which
// every command treats as "use the default" except where validation rejects it.

using System;
using System.Collections.Generic;
using System.IO;
using System.Reflection;
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
        /// Nodes walked before the walk stops.
        internal const int SceneNodeCountMax = 500;
        /// Hierarchy page size when the caller omits limit.
        internal const int ScenePageDefault = 500;
        /// Console entries when the caller omits maxEntries.
        internal const int ConsoleDefault = 100;
        /// Console entries per read at most.
        internal const int ConsoleMax = 500;
        /// Console messages are truncated past this length; flagged per entry.
        internal const int ConsoleMessageMax = 2000;
        /// Build scenes listed before the list is truncated.
        internal const int BuildScenesMax = 100;

        [Serializable]
        class Args
        {
            public long offset;
            public long limit;
            public string path;
            public long maxEntries;
        }

        [Serializable]
        class Incoming
        {
            public long id;
            public string command;
            public Args args;
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
            var args = incoming.args ?? new Args();
            try
            {
                switch (incoming.command)
                {
                    case "editor_state":
                        return Success(incoming.id, EditorState());
                    case "scene_hierarchy":
                        return Success(incoming.id, SceneHierarchy(args.offset, args.limit));
                    case "gameobject_properties":
                        return Success(incoming.id, GameObjectProperties(args.path));
                    case "console":
                        return Success(incoming.id, Console(args.maxEntries));
                    case "project_info":
                        return Success(incoming.id, ProjectInfo());
                    case "selection":
                        return Success(incoming.id, SelectionInfo());
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
        // Paging slices the walked list; defaults (offset 0, limit 0) return
        // the whole walked list, matching the U0 response plus new fields.
        static string SceneHierarchy(long offset, long limit)
        {
            if (offset < 0)
                throw new ArgumentException("offset must be >= 0");
            var effectiveLimit = limit == 0 ? ScenePageDefault : limit;
            if (effectiveLimit < 1 || effectiveLimit > SceneNodeCountMax)
                throw new ArgumentException("limit must be between 1 and " + SceneNodeCountMax);
            var scene = SceneManager.GetActiveScene();
            var roots = scene.GetRootGameObjects();
            var stack = new Stack<(Transform, int)>();
            for (var index = roots.Length - 1; index >= 0; index--)
                stack.Push((roots[index].transform, 0));
            var walked = new List<string>();
            var truncatedByDepth = false;
            while (stack.Count > 0)
            {
                if (walked.Count == SceneNodeCountMax)
                    break;
                var (transform, depth) = stack.Pop();
                var node = new StringBuilder("{");
                node.Append("\"name\":").Append(Quote(transform.name));
                node.Append(",\"active\":").Append(Bool(transform.gameObject.activeSelf));
                node.Append(",\"childCount\":").Append(transform.childCount);
                node.Append(",\"depth\":").Append(depth).Append('}');
                walked.Add(node.ToString());
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
            var totalCount = walked.Count;
            var page = new StringBuilder();
            var pageCount = 0;
            for (var index = offset; index < totalCount && pageCount < effectiveLimit; index++)
            {
                if (pageCount > 0)
                    page.Append(',');
                page.Append(walked[(int)index]);
                pageCount++;
            }
            var json = new StringBuilder("{");
            json.Append("\"scene\":{\"name\":").Append(Quote(scene.name));
            json.Append(",\"path\":").Append(Quote(scene.path)).Append('}');
            json.Append(",\"rootCount\":").Append(roots.Length);
            json.Append(",\"depthMax\":").Append(SceneDepthMax);
            json.Append(",\"nodeCountMax\":").Append(SceneNodeCountMax);
            json.Append(",\"nodeCount\":").Append(pageCount);
            json.Append(",\"totalCount\":").Append(totalCount);
            json.Append(",\"offset\":").Append(offset);
            json.Append(",\"limit\":").Append(effectiveLimit);
            json.Append(",\"truncatedByDepth\":").Append(Bool(truncatedByDepth));
            json.Append(",\"truncatedByCount\":").Append(Bool(stack.Count > 0));
            json.Append(",\"pageTruncated\":").Append(Bool(offset + pageCount < totalCount));
            json.Append(",\"onMainThread\":").Append(Bool(OnMainThread()));
            json.Append(",\"nodes\":[").Append(page).Append("]");
            return json.Append('}').ToString();
        }

        static string GameObjectProperties(string path)
        {
            if (string.IsNullOrEmpty(path))
                throw new ArgumentException("path is required");
            var segments = path.Split('/');
            foreach (var segment in segments)
                if (string.IsNullOrEmpty(segment))
                    throw new ArgumentException("path must not contain empty segments");
            var found = FindByPath(segments);
            if (found == null)
                throw new InvalidOperationException("gameobject not found: " + path);
            var transform = found.transform;
            var components = new StringBuilder();
            var all = found.GetComponents<Component>();
            for (var index = 0; index < all.Length; index++)
            {
                if (index > 0)
                    components.Append(',');
                components.Append(Quote(all[index] == null ? "" : all[index].GetType().FullName));
            }
            var json = new StringBuilder("{");
            json.Append("\"path\":").Append(Quote(FullPath(transform)));
            json.Append(",\"name\":").Append(Quote(found.name));
            json.Append(",\"activeSelf\":").Append(Bool(found.activeSelf));
            json.Append(",\"activeInHierarchy\":").Append(Bool(found.activeInHierarchy));
            json.Append(",\"tag\":").Append(Quote(found.tag));
            json.Append(",\"layer\":").Append(Quote(LayerMask.LayerToName(found.gameObject.layer)));
            json.Append(",\"localPosition\":").Append(Vector(transform.localPosition));
            json.Append(",\"localRotationEuler\":").Append(Vector(transform.localRotation.eulerAngles));
            json.Append(",\"localScale\":").Append(Vector(transform.localScale));
            json.Append(",\"worldPosition\":").Append(Vector(transform.position));
            json.Append(",\"componentTypes\":[").Append(components).Append("]");
            json.Append(",\"childCount\":").Append(transform.childCount);
            json.Append(",\"onMainThread\":").Append(Bool(OnMainThread()));
            return json.Append('}').ToString();
        }

        static GameObject FindByPath(string[] segments)
        {
            GameObject current = null;
            foreach (var root in SceneManager.GetActiveScene().GetRootGameObjects())
            {
                if (root.name == segments[0])
                {
                    current = root;
                    break;
                }
            }
            if (current == null)
                return null;
            for (var level = 1; level < segments.Length; level++)
            {
                var next = (Transform)null;
                var parent = current.transform;
                for (var child = 0; child < parent.childCount; child++)
                {
                    if (parent.GetChild(child).name == segments[level])
                    {
                        next = parent.GetChild(child);
                        break;
                    }
                }
                if (next == null)
                    return null;
                current = next.gameObject;
            }
            return current;
        }

        // Editor console via UnityEditor.LogEntries reflection: the type is
        // internal, so every lookup is checked and a missing member is an
        // editor error, never an empty list. Entries are newest first.
        // LogEntry.mode is reported raw: its bit meanings are Unity-internal
        // and this code does not guess them.
        static string Console(long maxEntries)
        {
            if (maxEntries < 0)
                throw new ArgumentException("maxEntries must be >= 0");
            var effective = maxEntries == 0 ? ConsoleDefault : maxEntries;
            if (effective < 1 || effective > ConsoleMax)
                throw new ArgumentException("maxEntries must be between 1 and " + ConsoleMax);
            var assembly = typeof(UnityEditor.Editor).Assembly;
            var logEntries = assembly.GetType("UnityEditor.LogEntries");
            var logEntry = assembly.GetType("UnityEditor.LogEntry");
            if (logEntries == null || logEntry == null)
                throw new InvalidOperationException("UnityEditor.LogEntries not found in this Unity version");
            var flags = BindingFlags.Static | BindingFlags.Public | BindingFlags.NonPublic;
            var getCount = logEntries.GetMethod("GetCount", flags);
            var getEntry = logEntries.GetMethod("GetEntryInternal", flags);
            if (getCount == null || getEntry == null)
                throw new InvalidOperationException("UnityEditor.LogEntries lacks GetCount/GetEntryInternal in this Unity version");
            var total = (int)getCount.Invoke(null, null);
            var entries = new StringBuilder();
            var returned = 0;
            for (var index = total - 1; index >= 0 && returned < effective; index--)
            {
                var entry = Activator.CreateInstance(logEntry, true);
                getEntry.Invoke(null, new object[] { index, entry });
                var message = EntryString(logEntry, entry, "message");
                var truncated = false;
                if (message.Length > ConsoleMessageMax)
                {
                    message = message.Substring(0, ConsoleMessageMax);
                    truncated = true;
                }
                if (returned > 0)
                    entries.Append(',');
                entries.Append("{\"message\":").Append(Quote(message));
                entries.Append(",\"file\":").Append(Quote(EntryString(logEntry, entry, "file")));
                entries.Append(",\"line\":").Append(EntryInt(logEntry, entry, "line"));
                entries.Append(",\"mode\":").Append(EntryInt(logEntry, entry, "mode"));
                entries.Append(",\"identifier\":").Append(EntryInt(logEntry, entry, "identifier"));
                entries.Append(",\"messageTruncated\":").Append(Bool(truncated)).Append('}');
                returned++;
            }
            var json = new StringBuilder("{");
            json.Append("\"totalCount\":").Append(total);
            json.Append(",\"returnedCount\":").Append(returned);
            json.Append(",\"truncated\":").Append(Bool(total > returned));
            json.Append(",\"onMainThread\":").Append(Bool(OnMainThread()));
            json.Append(",\"entries\":[").Append(entries).Append("]");
            return json.Append('}').ToString();
        }

        static string EntryString(Type logEntry, object entry, string field)
        {
            var info = logEntry.GetField(field, BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic);
            if (info == null)
                return "";
            return info.GetValue(entry) as string ?? "";
        }

        static int EntryInt(Type logEntry, object entry, string field)
        {
            var info = logEntry.GetField(field, BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic);
            if (info == null)
                return 0;
            var value = info.GetValue(entry);
            return value is int number ? number : 0;
        }

        static string ProjectInfo()
        {
            var projectPath = Path.GetDirectoryName(Application.dataPath);
            var scene = SceneManager.GetActiveScene();
            var buildScenes = EditorBuildSettings.scenes;
            var listed = new StringBuilder();
            var shown = 0;
            for (; shown < buildScenes.Length && shown < BuildScenesMax; shown++)
            {
                if (shown > 0)
                    listed.Append(',');
                listed.Append("{\"path\":").Append(Quote(buildScenes[shown].path));
                listed.Append(",\"enabled\":").Append(Bool(buildScenes[shown].enabled)).Append('}');
            }
            var json = new StringBuilder("{");
            json.Append("\"productName\":").Append(Quote(PlayerSettings.productName));
            json.Append(",\"companyName\":").Append(Quote(PlayerSettings.companyName));
            json.Append(",\"bundleVersion\":").Append(Quote(PlayerSettings.bundleVersion));
            json.Append(",\"unityVersion\":").Append(Quote(Application.unityVersion));
            json.Append(",\"projectName\":").Append(Quote(Path.GetFileName(projectPath)));
            json.Append(",\"projectPath\":").Append(Quote(projectPath));
            json.Append(",\"activeBuildTarget\":").Append(Quote(EditorUserBuildSettings.activeBuildTarget.ToString()));
            json.Append(",\"activeScene\":{\"name\":").Append(Quote(scene.name));
            json.Append(",\"path\":").Append(Quote(scene.path)).Append('}');
            json.Append(",\"buildSceneCount\":").Append(buildScenes.Length);
            json.Append(",\"buildScenesTruncated\":").Append(Bool(buildScenes.Length > shown));
            json.Append(",\"onMainThread\":").Append(Bool(OnMainThread()));
            json.Append(",\"buildScenes\":[").Append(listed).Append("]");
            return json.Append('}').ToString();
        }

        static string SelectionInfo()
        {
            var selected = Selection.gameObjects;
            var objects = new StringBuilder();
            for (var index = 0; index < selected.Length; index++)
            {
                if (index > 0)
                    objects.Append(',');
                objects.Append("{\"name\":").Append(Quote(selected[index].name));
                objects.Append(",\"path\":").Append(Quote(FullPath(selected[index].transform)));
                objects.Append(",\"activeSelf\":").Append(Bool(selected[index].activeSelf)).Append('}');
            }
            var active = Selection.activeGameObject;
            var others = Selection.objects.Length - selected.Length;
            var json = new StringBuilder("{");
            json.Append("\"selectedCount\":").Append(selected.Length);
            json.Append(",\"activeObjectName\":").Append(NullQuote(active == null ? null : active.name));
            json.Append(",\"otherSelectedCount\":").Append(others < 0 ? 0 : others);
            json.Append(",\"onMainThread\":").Append(Bool(OnMainThread()));
            json.Append(",\"gameObjects\":[").Append(objects).Append("]");
            return json.Append('}').ToString();
        }

        static string FullPath(Transform transform)
        {
            var names = new List<string>();
            for (var current = transform; current != null; current = current.parent)
                names.Add(current.name);
            names.Reverse();
            return string.Join("/", names.ToArray());
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

        static string Vector(Vector3 value)
        {
            return "{\"x\":" + value.x.ToString("R") + ",\"y\":" + value.y.ToString("R") + ",\"z\":" + value.z.ToString("R") + "}";
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

        static string NullQuote(string value)
        {
            return value == null ? "null" : Quote(value);
        }
    }
}
