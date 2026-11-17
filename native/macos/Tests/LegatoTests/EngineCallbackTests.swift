import Foundation
import Testing
@testable import Legato

/// The engine calls back from its own threads. Swift stops the app if a main-actor
/// callback runs on any other thread, which made every launch crash as the engine
/// started.
@Suite struct EngineCallbackTests {
    @Test func testEngineCallbackRunsOnOtherThreads() async {
        // The same conversion `legato_set_observer` makes.
        let callback: @convention(c) () -> Void = engineChanged
        await Task.detached { callback() }.value
        let thread = Thread { callback() }
        thread.start()
        while !thread.isFinished { try? await Task.sleep(for: .milliseconds(5)) }
    }
}
