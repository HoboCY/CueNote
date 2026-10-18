import AppKit
import ApplicationServices
import Foundation

let args=Array(CommandLine.arguments.dropFirst())
do {
    guard ProcessInfo.processInfo.operatingSystemVersion.majorVersion>=14 else {throw Failure("macOS 14 or newer is required")}
    if ["matrix","actions"].contains(args.first ?? "") {
        // Initialize the CLI's display-server connection before hit testing.
        // This unbundled driver must retain its default nonactivating policy.
        guard NSApplication.shared.activationPolicy() == .prohibited else {throw Failure("Native driver must not activate")}
    }
    switch args.first ?? "" {
    case "doctor": try printJSON(["os":ProcessInfo.processInfo.operatingSystemVersionString,"accessibility":AXIsProcessTrusted(),"nativeInput":CGPreflightPostEventAccess(),"screenshots":CGPreflightScreenCaptureAccess()])
    case "launch":
        let run=try launch(Array(args.dropFirst()))
        try printJSON(["run":run,"pid":number(try read(run+"/launcher.json")["pid"])])
    case "matrix":
        guard args.count==3 else {throw Failure("matrix <kit directory> <new absolute output directory>")}
        try matrix(kit:args[1],base:args[2])
    case "prepare":
        guard args.count==3 else {throw Failure("prepare <verified corpus> <new absolute root>")}
        try prepare(corpus:args[1],destination:args[2])
    case "resources":
        guard args.count==3 else {throw Failure("resources <kit directory> <new absolute output directory>")}
        try resources(kit:args[1],base:args[2])
    case "inspect","actions","close","capture","protocol-check":
        guard args.count>=2 else {throw Failure("Missing run directory")}
        let driver=try Driver(run:args[1])
        do {
            switch args[0] {
            case "inspect": try printJSON(driver.snapshot())
            case "actions": guard args.count==3 else {throw Failure("Missing action plan")}; try driver.actions(args[2])
            case "close": try driver.close()
            case "capture": guard args.count==3 else {throw Failure("Missing capture name")}; try driver.capture(args[2])
            default:
                let refused: [(Object,String)] = [
                    (["token":"wrong","operation":"snapshot"],"Unauthorized observation"),
                    (["token":driver.token,"operation":"eval","script":"document.body.textContent='invalid'"],"Invalid observation request"),
                    (["token":driver.token,"operation":"snapshot","script":"invalid"],"Invalid observation request")
                ]
                for (payload, expectedError) in refused {
                    do {_ = try driver.request("snapshot",override:payload); throw Failure("Invalid request was accepted")}
                    catch let failure as Failure {if failure.description != expectedError {throw failure}}
                }
                let before=monotonicMs(); let painted=try driver.snapshot(true)
                guard number(painted["observedFrames"])==2 else {throw Failure("Async observation did not await both frames")}
                let paintReadMs=monotonicMs()-before
                let geometry=try object(driver.request("snapshot")["screen"])
                try printJSON(["invalidRequestsRejected":true,"asyncPaintReadMs":paintReadMs,"paintedSnapshot":painted,"snapshot":driver.snapshot(),"screen":geometry])
            }
        } catch {try? append(args[1]+"/failures.jsonl",["command":args[0],"error":String(describing:error)]); throw error}
    default: throw Failure("Use doctor, launch, inspect, actions, close, capture or protocol-check")
    }
} catch {fputs("\(error)\n",stderr); exit(1)}
