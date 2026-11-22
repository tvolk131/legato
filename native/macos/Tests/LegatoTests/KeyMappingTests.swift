import Foundation
import Testing
@testable import Legato

@Suite struct KeyMappingTests {
    @Test func testMacKeyLookupCoversModifiersFnAndLetters() {
        #expect(KeyTargets.macKey(for: 63) == KeyOption(code: "Fn", label: "fn (Globe)"))
        #expect(KeyTargets.macKey(for: 57) == KeyOption(code: "CapsLock", label: "Caps Lock"))
        #expect(KeyTargets.macKey(for: 55) == KeyOption(code: "MetaLeft", label: "Left Command"))
        #expect(KeyTargets.macKey(for: 54) == KeyOption(code: "MetaRight", label: "Right Command"))
        #expect(KeyTargets.macKey(for: 62) == KeyOption(code: "ControlRight", label: "Right Control"))
        #expect(KeyTargets.macKey(for: 0) == KeyOption(code: "KeyA", label: "A"))
        #expect(KeyTargets.macKey(for: 0xffff) == nil)
    }

    @Test func testTargetListsMatchEachOperatingSystem() {
        let targets = KeyTargets.current
        #expect(targets.mac.first == KeyOption(code: "Fn", label: "fn (Globe)"))
        #expect(!targets.windows.contains { $0.code == "Fn" })
        #expect(targets.windows.contains(KeyOption(code: "ControlLeft", label: "Left Ctrl")))
        #expect(targets.mac.contains(KeyOption(code: "ControlLeft", label: "Left Control")))
    }

    @Test func testModifierPressDetectionDistinguishesPressFromRelease() {
        // Caps Lock always triggers on press.
        #expect(isModifierKeyDown(keyCode: 57, rawFlags: 0))
        // Right Control down vs up.
        #expect(isModifierKeyDown(keyCode: 62, rawFlags: 0x2000))
        #expect(!isModifierKeyDown(keyCode: 62, rawFlags: 0))
        // fn (Globe) down vs up.
        #expect(isModifierKeyDown(keyCode: 63, rawFlags: 0x0080_0000))
        #expect(!isModifierKeyDown(keyCode: 63, rawFlags: 0))
        // Unrelated key code.
        #expect(!isModifierKeyDown(keyCode: 0, rawFlags: 0xffff_ffff))
    }
}
