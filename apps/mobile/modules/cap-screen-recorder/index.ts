export type {
	CapScreenRecorderAvailability,
	CapScreenRecorderSegment,
	CapScreenRecorderStatus,
	CapScreenRecorderUpdates,
	CapScreenRecorderViewProps,
	PrepareScreenRecordingOptions,
} from "./src/CapScreenRecorder.types";
export {
	cancelScreenRecording,
	getScreenRecordingAvailability,
	getScreenRecordingStatus,
	getScreenRecordingUpdates,
	prepareScreenRecording,
} from "./src/CapScreenRecorderModule";
export { default as CapScreenRecorderView } from "./src/CapScreenRecorderView";
