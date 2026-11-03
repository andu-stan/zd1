// What the page's modules share and change: the protocol's tables (`spec()`), the connection (our firmware's or the
// bootloader's), the speaker's state as last read, the job's stop request, the files picked for a rollback.
export const app = {
  S: null, // spec(): the enums, the settings, the protocol version
  sp: null, // Speaker: our firmware
  bl: null, // Bootloader: the speaker in DFU
  busy: 0, // operations running
  info: null, // the last InfoReply
  state: null, // the last State record
  known: [], // the known Wi-Fi networks
  cancel: false, // the job's Stop was pressed
  rbBcd: null, // the rollback's files: { name, data }
  rbMcu: null,
  drawnFor: "", // the versions the releases were drawn for
  led: 0, // Diag's `led`: what the LED shows (led.js)
};
