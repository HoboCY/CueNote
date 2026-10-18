import AppKit
import ApplicationServices
import Foundation

let args=Array(CommandLine.arguments.dropFirst())
var active:Driver?, overlay:NSPanel?
do {
    guard args.count==3 else {throw Failure("guard-check <kit> <new root> <driver>")}
    let kit=args[0], root=args[1], executable=args[2]
    _ = NSApplication.shared
    NSApp.setActivationPolicy(.accessory)
    try directory(root); try directory(root+"/library")
    try createExclusive(root+"/.cuenote-performance",Data(marker.utf8))
    let exe=kit+"/CueNote Performance.app/Contents/MacOS/cuenote"
    let run=try launch([root,exe,text(try object(read(kit+"/receipt.json")["binaries"])["CueNote Performance.app/Contents/MacOS/cuenote"]),"welcome"])
    let driver=try Driver(run:run); active=driver
    let selector=".empty-actions .primary-button"
    try write(root+"/click.json",[["name":"pointer-guard-regression","actions":[["kind":"click","selector":selector]]]])
    func click(_ name:String) throws -> (Int32,String) {
        let log=root+"/"+name+".txt"; try createExclusive(log,Data())
        let output=try FileHandle(forWritingTo:URL(fileURLWithPath:log)); defer {try? output.close()}
        let process=Process(); process.executableURL=URL(fileURLWithPath:executable)
        process.arguments=["actions",run,root+"/click.json"]; process.standardOutput=output; process.standardError=output
        try process.run(); process.waitUntilExit()
        guard process.terminationReason == .exit else {throw Failure("Input CLI exited abnormally")}
        return (process.terminationStatus,try String(contentsOfFile:log,encoding:.utf8))
    }
    let geometry=try driver.request("element",selector:selector)
    let element=try object(geometry["value"]), rect=try object(element["rect"])
    let point=try driver.point(geometry,x:number(rect["x"])+number(rect["width"])/2,y:number(rect["y"])+number(rect["height"])/2)
    guard let primary=NSScreen.screens.first else {throw Failure("No primary screen")}
    let panel=NSPanel(contentRect:CGRect(x:point.x-35,y:primary.frame.maxY-point.y-35,width:70,height:70),
        styleMask:[.borderless,.nonactivatingPanel],backing:.buffered,defer:false)
    overlay=panel; panel.level = .floating; panel.backgroundColor = .systemOrange
    panel.isOpaque=true; panel.hidesOnDeactivate=false
    panel.orderFrontRegardless()
    RunLoop.current.run(until:Date(timeIntervalSinceNow:0.2))
    try driver.foreground()
    let (coveredExit,message)=try click("covered-command")
    guard coveredExit==1, message.contains("Native pointer target mismatch") else {throw Failure("Covered CLI did not reject input: \(message)")}
    try write(root+"/covered-rejection.json",["error":message,"overlayPID":ProcessInfo.processInfo.processIdentifier,"overlayWindow":panel.windowNumber,"exitCode":coveredExit])
    let covered=try driver.snapshot()
    try write(root+"/covered-state.json",covered)
    guard yes(covered["focused"]), text(covered["text"]).contains("选择你的笔记库") else {throw Failure("Covered click changed the application")}
    panel.orderOut(nil); panel.close(); overlay=nil
    RunLoop.current.run(until:Date(timeIntervalSinceNow:0.2))
    let (uncoveredExit,uncoveredLog)=try click("uncovered-command")
    guard uncoveredExit==0 else {throw Failure("Uncovered CLI failed: \(uncoveredLog)")}
    var dismissed=false
    let deadline=monotonicMs()+5000
    while monotonicMs()<deadline {
        let state=try driver.snapshot()
        if yes(state["focused"]), text(state["visibility"])=="visible", !text(state["text"]).contains("选择你的笔记库"), (state["errors"] as? [String] ?? []).isEmpty {
            try write(root+"/uncovered-state.json",state); dismissed=true; break
        }
        pause(20)
    }
    guard dismissed else {throw Failure("Uncovered click did not open the empty library")}
    try driver.close(); active=nil
    try write(root+"/result.json",["coveredClickRejected":true,"coveredWelcomeUnchanged":true,"uncoveredClickSucceeded":true,"run":run,"normalExitVerified":true,"actualDriverSha256":try hash(executable),"inputViaActualCLI":true])
} catch {
    overlay?.orderOut(nil); overlay?.close()
    if let active {try? active.close()}
    fputs("\(error)\n",stderr); exit(1)
}
