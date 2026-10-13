package org.jetbrains.plugins.template.listener

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class StackTraceAnalyzerTest {

    @Test
    fun packageMatchedCopilotStacksRemainCheckpointEligible() {
        val stack = arrayOf(
            StackTraceElement(
                "com.github.copilot.inline.CopilotEditorAction",
                "applyEdit",
                "CopilotEditorAction.kt",
                42
            )
        )

        val result = StackTraceAnalyzer.analyze(stack)

        assertEquals("github-copilot-jetbrains", result.sourceName)
        assertEquals(StackTraceAnalyzer.Confidence.HIGH, result.confidence)
        assertTrue(StackTraceAnalyzer.shouldTriggerCheckpoint(result))
    }

    @Test
    fun classOnlyCopilotStacksStillTriggerCheckpoints() {
        val stack = arrayOf(
            StackTraceElement(
                "org.example.copilotbridge.CopilotPatchExecutor",
                "applyPatch",
                "CopilotPatchExecutor.kt",
                18
            )
        )

        val result = StackTraceAnalyzer.analyze(stack)

        assertEquals("github-copilot-jetbrains", result.sourceName)
        assertEquals(StackTraceAnalyzer.Confidence.MEDIUM, result.confidence)
        assertTrue(StackTraceAnalyzer.shouldTriggerCheckpoint(result))
    }

    @Test
    fun nonCopilotMediumMatchesStayRejected() {
        val stack = arrayOf(
            StackTraceElement(
                "org.example.matterhornproxy.SomeProxy",
                "rewriteDocument",
                "SomeProxy.kt",
                9
            )
        )

        val result = StackTraceAnalyzer.analyze(stack)

        assertEquals("junie", result.sourceName)
        assertEquals(StackTraceAnalyzer.Confidence.MEDIUM, result.confidence)
        assertFalse(StackTraceAnalyzer.shouldTriggerCheckpoint(result))
    }

    @Test
    fun unrelatedStacksDoNotTriggerCheckpoints() {
        val stack = arrayOf(
            StackTraceElement(
                "com.example.editor.ManualEditHandler",
                "apply",
                "ManualEditHandler.kt",
                7
            )
        )

        val result = StackTraceAnalyzer.analyze(stack)

        assertEquals(null, result.sourceName)
        assertEquals(StackTraceAnalyzer.Confidence.NONE, result.confidence)
        assertFalse(StackTraceAnalyzer.shouldTriggerCheckpoint(result))
    }
}