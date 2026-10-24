// Graceful editor shutdown for the tackle smoke runs (U1 lesson from U0:
// stopping the Editor with a signal segfaulted it and opened a crash
// reporter dialog). Run: Unity -executeMethod Tackle.Editor.TackleLifecycle.Exit
// No MCP tool calls this; it exists so scripts can quit cleanly.

using UnityEditor;

namespace Tackle.Editor
{
    public static class TackleLifecycle
    {
        public static void Exit()
        {
            EditorApplication.Exit(0);
        }
    }
}
