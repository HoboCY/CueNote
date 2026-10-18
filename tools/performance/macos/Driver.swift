// Native input and external monotonic timing for the isolated CueNote app.
// The observer accepts fixed reads only. No DOM writes or business IPC calls.
import AppKit
import ApplicationServices
import CryptoKit
import Darwin
import Foundation
import ImageIO
import Security
import UniformTypeIdentifiers

typealias Object = [String: Any]
struct Failure: Error, CustomStringConvertible { let description: String; init(_ text: String) { description = text } }
let files = FileManager.default
let marker = "CueNote isolated performance root v1\n"
// Matrix launches retain Process objects so every restart can verify an exit
// code. A separate CLI invocation must explicitly report status as unavailable.
var launchedProcesses: [pid_t:Process] = [:]
func fail(_ message: String) throws -> Never { throw Failure(message) }
func object(_ value: Any?) throws -> Object { guard let value = value as? Object else { throw Failure("Expected JSON object") }; return value }
func text(_ value: Any?) -> String { value as? String ?? "" }
func number(_ value: Any?) -> Double { (value as? NSNumber)?.doubleValue ?? 0 }
func yes(_ value: Any?) -> Bool { (value as? NSNumber)?.boolValue ?? false }
func monotonicMs() -> Double { Double(DispatchTime.now().uptimeNanoseconds) / 1_000_000 }
func pause(_ ms: Double) { Thread.sleep(forTimeInterval: ms / 1000) }
func json(_ value: Any) throws -> Data { try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]) }
func read(_ path: String) throws -> Object { try object(JSONSerialization.jsonObject(with: Data(contentsOf: URL(fileURLWithPath: path)))) }
func hash(_ path: String) throws -> String {
    let input = try FileHandle(forReadingFrom: URL(fileURLWithPath: path)); defer { try? input.close() }
    var hasher = SHA256()
    while let chunk = try input.read(upToCount: 1024 * 1024), !chunk.isEmpty { hasher.update(data: chunk) }
    return hasher.finalize().map { String(format: "%02x", $0) }.joined()
}
func write(_ path: String, _ value: Any) throws {
    try createExclusive(path,try json(value))
}
func createExclusive(_ path: String, _ data: Data) throws {
    try localPath(URL(fileURLWithPath:path).deletingLastPathComponent().path)
    let fd=Darwin.open(path,O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW,mode_t(0o600))
    guard fd>=0 else {throw Failure("Cannot exclusively create evidence: "+path)}
    let output=FileHandle(fileDescriptor:fd,closeOnDealloc:true); defer {try? output.close()}
    try output.write(contentsOf:data)
}
func append(_ path: String, _ value: Any) throws {
    try localPath(path)
    let fd=Darwin.open(path,O_WRONLY|O_CREAT|O_APPEND|O_NOFOLLOW,mode_t(0o600))
    guard fd>=0 else {throw Failure("Cannot append evidence")}
    let output=FileHandle(fileDescriptor:fd,closeOnDealloc:true); defer {try? output.close()}
    guard (try files.attributesOfItem(atPath:path)[.type] as? FileAttributeType) == .typeRegular else {throw Failure("Evidence must be a regular file")}
    try output.write(contentsOf: json(value)); try output.write(contentsOf: Data([10]))
}
func directory(_ path: String) throws {
    guard path.hasPrefix("/"), !files.fileExists(atPath: path) else { throw Failure("A new absolute directory is required: \(path)") }
    try files.createDirectory(atPath: path, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
}
func ownedRoot(_ root: String) throws {
    try localPath(root); try localPath(root+"/.cuenote-performance")
    guard root.hasPrefix("/"), URL(fileURLWithPath: root).resolvingSymlinksInPath().path == root,
          (try files.attributesOfItem(atPath:root+"/.cuenote-performance")[.type] as? FileAttributeType) == .typeRegular,
          try String(contentsOfFile: root + "/.cuenote-performance", encoding: .utf8) == marker else { throw Failure("Unowned or symbolic performance root") }
}
func printJSON(_ value: Any) throws { print(String(decoding: try json(value), as: UTF8.self)) }

final class Driver {
    let run: String, record: Object, pid: pid_t, token: String, port: UInt16
    private var pointerWindow: CGWindowID?
    private var pointerButtonDown = false
    init(run: String) throws {
        try localPath(run); try localPath(run+"/launcher.json"); try localPath(run+"/.observer-token")
        self.run = run; record = try read(run + "/launcher.json")
        guard URL(fileURLWithPath:run).deletingLastPathComponent().path == text(record["root"])+"/measurements",
              text(record["run"])==run else {throw Failure("Run is outside its owned measurement directory")}
        pid = pid_t(number(record["pid"]))
        token = try String(contentsOfFile: run + "/.observer-token", encoding: .utf8)
        let endpointPath=text(record["root"]) + "/observer-\(pid).json"; try localPath(endpointPath)
        let endpoint = try read(endpointPath)
        guard number(endpoint["pid"]) == Double(pid), let value = endpoint["port"] as? UInt16 else { throw Failure("Observer ownership mismatch") }
        port = value; try assertOwner()
    }
    func assertOwner() throws {
        try ownedRoot(text(record["root"]))
        // libproc's PROC_PIDPATHINFO_MAXSIZE is an expression macro, which the
        // Swift importer does not expose. Its SDK definition is 4 * MAXPATHLEN.
        var path = [CChar](repeating: 0, count: 4 * Int(MAXPATHLEN))
        guard pid > 0, proc_pidpath(pid, &path, UInt32(path.count)) > 0,
              String(cString: path) == text(record["exe"]), try hash(text(record["exe"])) == text(record["sha256"]) else { throw Failure("Application process or binary ownership changed") }
    }
    func foreground() throws {
        guard NSWorkspace.shared.frontmostApplication?.processIdentifier == pid else { throw Failure("CueNote lost foreground focus; measurement stopped") }
    }
    func requireInput() throws {
        guard AXIsProcessTrusted(), CGPreflightPostEventAccess() else { throw Failure("Native input permission is unavailable. Allow the driver/Terminal in macOS Accessibility, then start a new run; no JS input fallback.") }
    }
    func request(_ operation: String, selector: String? = nil, paint: Bool = false, override: Object? = nil) throws -> Object {
        let fd = socket(AF_INET, SOCK_STREAM, 0); guard fd >= 0 else { throw Failure("Cannot create observation socket") }; defer { Darwin.close(fd) }
        var timeout = timeval(tv_sec: 35, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
        var noSignal: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &noSignal, socklen_t(MemoryLayout<Int32>.size))
        var address = sockaddr_in(); address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET); address.sin_port = port.bigEndian
        address.sin_addr = in_addr(s_addr: inet_addr("127.0.0.1"))
        let connected = withUnsafePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { Darwin.connect(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) }
        }
        guard connected == 0 else { throw Failure("Cannot connect to the owned observer") }
        var payload: Object = ["token":token,"operation":operation,"paint":paint]
        if let selector { payload["selector"] = selector }
        if let override { payload = override }
        var data = try json(payload); data.append(10)
        try data.withUnsafeBytes { buffer in
            var offset = 0
            while offset < buffer.count {
                let sent = Darwin.send(fd, buffer.baseAddress!.advanced(by: offset), buffer.count-offset, 0)
                if sent <= 0 { throw Failure("Observation request write failed") }; offset += sent
            }
        }
        var response = Data(), buffer = [UInt8](repeating: 0, count: 8192)
        while response.last != 10 {
            let count = Darwin.recv(fd, &buffer, buffer.count, 0)
            guard count > 0, response.count + count <= 2 * 1024 * 1024 else { throw Failure("Observation response timeout/size failure") }
            response.append(contentsOf: buffer.prefix(count))
        }
        let result = try object(JSONSerialization.jsonObject(with: response))
        guard number(result["pid"]) == Double(pid) else { throw Failure("Unexpected observer PID") }
        if result["error"] != nil { throw Failure(text(result["error"])) }
        return result
    }
    func snapshot(_ paint: Bool = false) throws -> Object { try object(request("snapshot", paint: paint)["value"]) }
    func point(_ response: Object, x: Double, y: Double) throws -> CGPoint {
        let screen = try object(response["screen"]), value = try object(response["value"]), viewport = try object(value["viewport"])
        let native = try object(screen["nativeGeometry"]), content = try object(native["safeAreaRect"])
        let layout = try object(native["windowContentLayoutRect"])
        for key in ["x","y","width","height"] {
            guard let observed=content[key] as? NSNumber, let expected=layout[key] as? NSNumber,
                  observed.doubleValue.isFinite, expected.doubleValue.isFinite,
                  abs(observed.doubleValue-expected.doubleValue)<=1 else {
                throw Failure("WebView safe area and window content layout differ")
            }
        }
        guard yes(screen["key"]), number(viewport["width"]) > 0, number(viewport["height"]) > 0,
              number(content["width"]) > 0, number(content["height"]) > 0,
              number(native["pageZoom"]) == 1, number(native["magnification"]) == 1,
              abs(number(content["width"])-number(viewport["width"])) <= 1,
              abs(number(content["height"])-number(viewport["height"])) <= 1,
              x.isFinite, y.isFinite else { throw Failure("Unsupported/unfocused WebView viewport geometry") }
        guard let window = screen["windowNumber"] as? NSNumber, window.uint32Value > 0 else { throw Failure("Missing native target window") }
        pointerWindow = window.uint32Value
        // WKWebView bounds include the title-bar area on macOS 15 and 26.
        // The observed CSS viewport matches the safe area, not those bounds.
        return CGPoint(x: number(content["x"]) + x * number(content["width"]) / number(viewport["width"]),
                       y: number(content["y"]) + y * number(content["height"]) / number(viewport["height"]))
    }
    func pointerTarget(_ point: CGPoint) throws {
        guard Thread.isMainThread, let expected = pointerWindow, let primary = NSScreen.screens.first else {
            throw Failure("Cannot verify the native pointer target")
        }
        // AppKit uses bottom-left coordinates and mouse-down hit testing,
        // excluding transparent points and windows that ignore mouse events.
        // Bounding rectangles alone misidentify the cursor and Dock helpers.
        let cocoaPoint = NSPoint(x:point.x,y:primary.frame.maxY-point.y)
        let hit = NSWindow.windowNumber(at:cocoaPoint,belowWindowWithWindowNumber:0)
        guard hit == Int(expected) else {
            throw Failure("Native pointer target mismatch: expected PID \(pid), window \(expected); observed window \(hit) at CG \(point), AppKit \(cocoaPoint)")
        }
        guard let windows = CGWindowListCopyWindowInfo(.optionIncludingWindow,expected) as? [[String:Any]],
              let window = windows.first,
              (window[kCGWindowOwnerPID as String] as? Int) == Int(pid),
              (window[kCGWindowNumber as String] as? Int) == Int(expected),
              (window[kCGWindowIsOnscreen as String] as? Bool) == true,
              let bounds = window[kCGWindowBounds as String] as? [String:Double],
              let x = bounds["X"], let y = bounds["Y"], let width = bounds["Width"], let height = bounds["Height"],
              CGRect(x:x,y:y,width:width,height:height).contains(point) else {
            throw Failure("Native pointer window ownership or geometry changed")
        }
    }
    func post(_ event: CGEvent?) throws {
        guard let event else { throw Failure("Cannot create native event") }
        do {
            try foreground()
            if [.mouseMoved,.leftMouseDown,.leftMouseUp,.scrollWheel].contains(event.type) {
                // Plans use unmodified pointer input. CGEvent otherwise inherits
                // the previous synthetic chord (e.g. Command-S), changing clicks.
                event.flags = []
                try pointerTarget(event.location)
            }
            // PID-directed mouse events did not reach WKWebView on the target M1.
            // Use the normal session event route after verifying focus and hit location.
            try foreground()
            event.post(tap:.cgSessionEventTap)
            if event.type == .leftMouseDown { pointerButtonDown = true }
            if event.type == .leftMouseUp { pointerButtonDown = false }
            try foreground()
        } catch {
            try? append(run+"/input-failures.jsonl",["eventType":event.type.rawValue,
                "pointerButtonMayRemainDown":pointerButtonDown,"error":String(describing:error)])
            throw error
        }
    }
    func wheel(_ response: Object, x: Double, y: Double, delta: Double) throws {
        let event = CGEvent(scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 1, wheel1: Int32(-delta.rounded()), wheel2: 0, wheel3: 0)
        event?.location = try point(response, x: x, y: y); try post(event)
    }
    func scroll(_ selector: String) throws {
        for _ in 0..<100 {
            try foreground()
            let response = try request("element", selector: selector), value = try object(response["value"])
            if yes(value["visible"]) { return }
            if yes(value["missing"]) { throw Failure("Scroll target is missing: \(selector)") }
            let target = try object(value["rect"]), viewport = try object(value["viewport"])
            // The observer returns nearest-to-outer ancestors. A nested list
            // must scroll itself once visible, not keep scrolling its sidebar.
            let containers = value["containers"] as? [Object] ?? []
            var destination: (Double,Double,Double)?
            for (index,container) in containers.enumerated() {
                var top=0.0, left=0.0, bottom=number(viewport["height"]), right=number(viewport["width"])
                for clip in containers[index...] {
                    top=max(top,number(clip["top"])); bottom=min(bottom,number(clip["bottom"]))
                    left=max(left,number(clip["left"])); right=min(right,number(clip["right"]))
                }
                guard bottom-top>20, right-left>20 else {continue}
                let delta=max(-800,min(800,number(target["top"])+number(target["height"])/2-(top+bottom)/2))
                let position=number(container["scrollTop"]), maximum=number(container["scrollHeight"])-number(container["clientHeight"])
                guard (delta>1 && position<maximum-1) || (delta < -1 && position>1) else {continue}
                destination=((left+right)/2,(top+bottom)/2,delta); break
            }
            // The document scrolls even when html/body use overflow: visible,
            // so it is not in the observer's overflow-container list.
            if destination == nil && containers.isEmpty {
                let centerX=number(target["left"])+number(target["width"])/2
                let centerY=number(target["top"])+number(target["height"])/2
                let height=number(viewport["height"])
                let document=try object(value["documentScroll"])
                let position=number(document["top"]), maximum=number(document["height"])-number(document["clientHeight"])
                if centerX>10, centerX<number(viewport["width"])-10,
                   (centerY<0 && position>1) || (centerY>=height && position<maximum-1) {
                    destination=(centerX,height/2,max(-800,min(800,centerY-height/2)))
                }
            }
            guard let (x,y,delta)=destination else {throw Failure("No visible native scroll container can expose target: "+selector)}
            try wheel(response,x:x,y:y,delta:delta)
            _ = try snapshot(true)
        }
        throw Failure("Native scrolling did not expose target")
    }
    func key(_ value: Object) throws {
        let name = text(value["code"]), modifiers = Int(number(value["modifiers"]))
        var flags: CGEventFlags = []
        if modifiers & 2 != 0 { flags.insert(.maskCommand) }
        if modifiers & 8 != 0 { flags.insert(.maskShift) }
        var codes: [String: CGKeyCode] = ["KeyA":0,"KeyS":1,"KeyQ":12,"Enter":36,"Escape":53,"Backspace":51,"Delete":117,"Tab":48,"ArrowLeft":123,"ArrowRight":124]
        // Windows Home/Shift+End plans mean beginning/end of the current line.
        if name == "Home" { codes[name] = 123; flags.insert(.maskCommand) }
        if name == "End" { codes[name] = 124; flags.insert(.maskCommand) }
        guard let code = codes[name] else { throw Failure("Unsupported native key: \(name)") }
        for down in [true,false] { let event = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: down); event?.flags = flags; try post(event) }
    }
    func action(_ value: Object) throws {
        try foreground()
        switch text(value["kind"]) {
        case "click":
            let response = try request("element", selector: text(value["selector"])), state = try object(response["value"])
            guard yes(state["visible"]) else { throw Failure("Click target is unavailable/obstructed") }
            guard state.keys.contains("textFocus") else {throw Failure("Observer lacks native text focus observations")}
            let rect = try object(state["rect"]), whereTo = try point(response, x:number(rect["x"])+number(rect["width"])/2, y:number(rect["y"])+number(rect["height"])/2)
            for kind in [CGEventType.mouseMoved,.leftMouseDown,.leftMouseUp] {
                let event=CGEvent(mouseEventSource:nil,mouseType:kind,mouseCursorPosition:whereTo,mouseButton:.left)
                if kind != .mouseMoved {event?.setIntegerValueField(.mouseEventClickState,value:1)}
                try post(event)
            }
            if state["textFocus"] is Object {
                let deadline=monotonicMs()+5000
                while true {
                    try foreground()
                    let current=try object(request("element",selector:text(value["selector"]),paint:true)["value"])
                    if let focus=current["textFocus"] as? Object, yes(focus["focused"]) {break}
                    guard !yes(current["missing"]), monotonicMs()<deadline else {throw Failure("Native text click did not acquire focus")}
                    pause(5)
                }
            }
        case "key": try key(value)
        case "text":
            let characters = Array(text(value["text"]).utf16)
            guard !characters.isEmpty, characters.count <= 1024 else { throw Failure("Native text event length is invalid") }
            for down in [true,false] {
                let event = CGEvent(keyboardEventSource:nil,virtualKey:0,keyDown:down)
                event?.flags = []; event?.keyboardSetUnicodeString(stringLength:characters.count,unicodeString:characters); try post(event)
            }
        case "scroll": try scroll(text(value["selector"]))
        case "request-close": guard NSRunningApplication(processIdentifier:pid)?.terminate() == true else { throw Failure("Normal close request failed") }
        default: throw Failure("Unsupported input action")
        }
    }
    func ready(_ state: Object, _ expected: Object) -> Bool {
        let kind = text(expected["kind"])
        switch kind {
        case "editor":
            let images=state["images"] as? [Object] ?? []
            return yes(state["editable"]) && text(state["noteId"]) == text(expected["noteId"]) && number(state["maps"]) > 0 && images.allSatisfy{!yes($0["visible"]) || yes($0["ready"])} && (!yes(expected["revealFirstImage"]) || (images.first.map{yes($0["ready"])} ?? false))
        case "index": return number(state["notes"]) == number(expected["count"]) && !(state["progress"] as? [String] ?? []).contains{ $0.contains("正在建立搜索索引") }
        case "save": return text(state["save"]) == "已保存到本地"
        case "map": return number(state["expandedNodes"]) == number(expected["count"]) && yes(state["mapCanEdit"])
        case "map-collapsed": return number(state["expandedNodes"]) == 0 && !yes(state["mapCanEdit"])
        case "title": return text(state["title"]) == text(expected["text"])
        case "dirty-title": return text(state["title"]) == text(expected["text"]) && text(state["save"]) == "立即保存"
        case "title-selection":
            guard let selection=state["titleSelection"] as? Object else {return false}
            return yes(selection["focused"]) && number(selection["start"])==0 && number(selection["end"])==Double(text(state["title"]).utf16.count)
        case "content-heading": return text(state["contentHeading"]) == text(expected["text"])
        case "content-heading-selection": return !text(state["contentHeading"]).isEmpty && text(state["selectedText"]) == text(state["contentHeading"])
        case "welcome": return text(state["text"]).contains("选择你的笔记库")
        case "transition": return text(state["transition"]).contains(text(expected["text"])) && (!yes(expected["saving"]) || text(state["save"]) == "正在保存…")
        case "transition-cancelled": return text(state["transition"]).isEmpty && text(state["noteId"]) == text(expected["noteId"]) && text(state["title"]) == text(expected["title"])
        case "search": return text(state["search"]).contains("共 \(Int(number(expected["total"]))) 条") && (state["hits"] as? [String] ?? []).sorted() == (expected["ids"] as? [String] ?? []).sorted()
        default: return false
        }
    }
    func until(_ expected: Object, timeout: Double = 240000, timings: inout Object) throws -> Object {
        let start=monotonicMs(); var revealed=false, state: Object=[:]
        while monotonicMs()-start < timeout {
            try foreground(); state=try snapshot()
            guard text(state["visibility"]) == "visible", yes(state["focused"]) else { throw Failure("Application document lost focus/visibility") }
            let elapsed=monotonicMs()-start
            for threshold in [1000,2000] where elapsed >= Double(threshold) && timings["feedback\(threshold)"] == nil {
                timings["feedback\(threshold)"]=["elapsedMs":elapsed,"save":state["save"] ?? NSNull(),"savingFeedback":state["savingFeedback"] ?? NSNull(),"progress":state["progress"] ?? NSNull()]
            }
            if yes(expected["revealFirstImage"]), !revealed, yes(state["editable"]), text(state["noteId"]) == text(expected["noteId"]) {
                try scroll(".rich-image-preview"); revealed=true; timings["firstImageScrolledMs"]=monotonicMs()-start; continue
            }
            if ready(state,expected) {
                state=try snapshot(true)
                try foreground()
                guard text(state["visibility"])=="visible", yes(state["focused"]), number(state["observedFrames"])==2 else {throw Failure("Paint completed without a focused visible document")}
                if ready(state,expected) {timings["readyMs"]=monotonicMs()-start; return state}
            }
            pause(5)
        }
        throw Failure("Expected state timed out: \(String(decoding:try json(["expected":expected,"state":state]),as:UTF8.self))")
    }
    func capture(_ name: String) throws {
        guard name.range(of:"^[a-zA-Z0-9-]+$",options:.regularExpression) != nil else { throw Failure("Unsafe capture name") }
        let response=try request("snapshot"), screen=try object(response["screen"])
        guard CGPreflightScreenCaptureAccess(), let image=CGWindowListCreateImage(.null,.optionIncludingWindow,CGWindowID(number(screen["windowNumber"])),[.boundsIgnoreFraming,.bestResolution]) else {
            try append(run+"/capture-unavailable.jsonl",["name":name,"reason":"Screen Recording permission or window image unavailable; no screenshot claimed"]); return
        }
        let url=URL(fileURLWithPath:run+"/"+name+".png")
        guard !files.fileExists(atPath:url.path), let destination=CGImageDestinationCreateWithURL(url as CFURL,UTType.png.identifier as CFString,1,nil) else {throw Failure("Cannot create capture")}
        CGImageDestinationAddImage(destination,image,nil); guard CGImageDestinationFinalize(destination) else {throw Failure("Cannot save capture")}
    }
    func actions(_ plan: String) throws {
        try assertOwner(); try requireInput()
        guard let steps=try JSONSerialization.jsonObject(with:Data(contentsOf:URL(fileURLWithPath:plan))) as? [Object] else {throw Failure("Invalid action plan")}
        for step in steps {
            var ignored: Object=[:]
            if let expected=step["beforeExpected"] as? Object {_ = try until(expected,timings:&ignored)}
            for setup in step["setup"] as? [Object] ?? [] {try action(setup)}
            if let expected=step["setupExpected"] as? Object {_ = try until(expected,timeout:5000,timings:&ignored)}
            try foreground(); let start=monotonicMs()
            for input in step["actions"] as? [Object] ?? [] {try action(input)}
            let inputMs=monotonicMs()-start; var timings: Object=[:]
            let state: Object
            if let expected=step["expected"] as? Object {state=try until(expected,timeout:step["timeoutMs"] == nil ? 240000 : number(step["timeoutMs"]),timings:&timings)}else{state=try snapshot()}
            var result: Object=["operation":text(step["name"]),"elapsedMs":monotonicMs()-start,"inputMs":inputMs,"readinessTimings":timings,"state":state]
            for key in ["metric","cacheState","expected"] {if let value=step[key] {result[key]=value}}
            try append(run+"/measurements.jsonl",result); try printJSON(["operation":result["operation"]!,"elapsedMs":result["elapsedMs"]!])
            if let capture=step["capture"] as? String {try self.capture(capture)}
        }
    }
    func close() throws {
        try assertOwner()
        guard NSRunningApplication(processIdentifier:pid)?.terminate() == true else {throw Failure("Normal quit was refused")}
        let end=monotonicMs()+60000
        if let process=launchedProcesses[pid] {
            while process.isRunning && monotonicMs()<end {pause(50)}
            guard !process.isRunning else {throw Failure("Application did not exit; not force-killed")}
            process.waitUntilExit()
            let normal=process.terminationReason == .exit && process.terminationStatus==0
            try write(run+"/closed.json",["pid":pid,"quitRequested":true,"exitStatusAvailable":true,
                "normalExit":normal,"terminationReason":process.terminationReason.rawValue,"status":process.terminationStatus])
            launchedProcesses.removeValue(forKey:pid)
            guard normal else {throw Failure("Application crashed or exited with nonzero status")}
        } else {
            while kill(pid,0)==0 && monotonicMs()<end {pause(50)}
            guard kill(pid,0) == -1, errno==ESRCH else {throw Failure("Process disappearance could not be verified")}
            try write(run+"/closed.json",["pid":pid,"quitRequested":true,"processDisappeared":true,
                "exitStatusAvailable":false,"note":"Separate CLI cannot observe the parent's exit status; normal exit is not asserted."])
        }
    }
}

func launch(_ args: [String]) throws -> String {
    guard args.count >= 3 else {throw Failure("launch <owned root> <absolute executable> <sha256> [welcome|editor|editor-first-image] [note ID]")}
    let root=args[0], exe=args[1], expectedHash=args[2], mode=args.count>3 ? args[3]:"welcome"
    guard ["welcome","index","editor","editor-first-image"].contains(mode), exe.hasPrefix("/"), try hash(exe)==expectedHash else {throw Failure("Invalid launch mode/binary hash")}
    try ownedRoot(root)
    let parent=root+"/measurements"; try localPath(parent)
    if !files.fileExists(atPath:parent) {try directory(parent)}
    let run=parent+"/"+UUID().uuidString; try directory(run)
    var secret=[UInt8](repeating:0,count:32)
    guard SecRandomCopyBytes(kSecRandomDefault,secret.count,&secret)==errSecSuccess else {throw Failure("Cannot generate observer credential")}
    let token=secret.map{String(format:"%02x",$0)}.joined()
    try createExclusive(run+"/.observer-token",Data(token.utf8))
    let process=Process(); process.executableURL=URL(fileURLWithPath:exe)
    var env=ProcessInfo.processInfo.environment
    for key in Array(env.keys) where key.hasPrefix("CUENOTE_FAULT_") || ["CUENOTE_TEST_ROOT","CUENOTE_PERF_TRACE","WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS"].contains(key) {env.removeValue(forKey:key)}
    env["CUENOTE_PERF_ROOT"]=root; env["CUENOTE_PERF_OBSERVER_TOKEN"]=token; process.environment=env
    for name in ["stdout","stderr"] {try createExclusive(run+"/"+name+".txt",Data())}
    let out=try FileHandle(forWritingTo:URL(fileURLWithPath:run+"/stdout.txt")), err=try FileHandle(forWritingTo:URL(fileURLWithPath:run+"/stderr.txt"))
    process.standardOutput=out; process.standardError=err
    let start=monotonicMs(); try process.run(); try out.close(); try err.close()
    let pid=process.processIdentifier
    launchedProcesses[pid]=process
    try write(run+"/launcher.json",["root":root,"exe":exe,"sha256":expectedHash,"pid":pid,"run":run,"variant":"perf-acceptance","traceEnabled":false,"platform":"macos","startedUtc":ISO8601DateFormatter().string(from:Date())])
    do {
        let endpoint=root+"/observer-\(pid).json", deadline=monotonicMs()+30000
        while (try? read(endpoint)) == nil && monotonicMs()<deadline {guard process.isRunning else {throw Failure("Application exited before observer startup")}; pause(10)}
        guard let metadata=try? read(endpoint) else {throw Failure("Observer startup timeout")}
        try write(run+"/observer.json",metadata)
        guard let app=NSRunningApplication(processIdentifier:pid), app.activate(options:[]) else {throw Failure("Could not activate native app")}
        while NSWorkspace.shared.frontmostApplication?.processIdentifier != pid && monotonicMs()<deadline {pause(10)}
        let driver=try Driver(run:run); var timings: Object=[:]
        // Activation and DOM focus are asynchronous. Startup time includes this
        // wait, but an already-running action never tolerates focus loss.
        while monotonicMs()<deadline {
            let state=try driver.snapshot()
            if text(state["visibility"])=="visible" && yes(state["focused"]) {break}
            pause(10)
        }
        let expected: Object = mode == "welcome" ? ["kind":"welcome"] : mode == "index" ? ["kind":"index","count":1000] : ["kind":"editor","noteId":args.count>4 ? args[4]:"","revealFirstImage":mode=="editor-first-image"]
        let state=try driver.until(expected,timings:&timings)
        try write(run+"/startup.json",["operation":mode.hasPrefix("editor") ? "startup":"empty-startup","readiness":mode,"elapsedMs":monotonicMs()-start,"readinessTimings":timings,"state":state])
        try driver.capture("startup")
        return run
    } catch {
        try? append(run+"/failures.jsonl",["command":"launch","error":String(describing:error)])
        if let driver=try? Driver(run:run) {try? driver.close()}
        else {
            _ = NSRunningApplication(processIdentifier:pid)?.terminate()
            let deadline=monotonicMs()+60000
            while process.isRunning && monotonicMs()<deadline {pause(50)}
            try? write(run+"/launch-failure-exit.json",["stillRunning":process.isRunning,
                "note":"Quit requested after startup failure; no successful startup claimed"])
        }
        throw error
    }
}
