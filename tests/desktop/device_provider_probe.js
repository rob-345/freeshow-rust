return (async () => {
  const { expectedIdentity, phase } = arguments[0];
  const context = window.api.getRuntimeContext();
  const result = { phase, context, callbacks: [] };
  const listener = "device-provider-probe-" + phase;
  const ids = {
    DEVICE_ID: listener + "-identity",
    PROVIDER_CONNECTIONS: listener + "-providers",
  };
  let timer;
  try {
    await new Promise((resolve, reject) => {
      timer = setTimeout(
        () => reject(new Error("Device/provider retained callback deadline")),
        4000,
      );
      window.api.receive(
        "MAIN",
        (message, correlation) => {
          if (correlation !== ids[message.channel]) return;
          if (result.callbacks.includes(message.channel))
            return reject(new Error("Duplicate device/provider callback"));
          if (message.channel === "DEVICE_ID") {
            // Never copy native identity bytes into the durable receipt.
            result.identity = {
              equalsPersisted: message.data === expectedIdentity,
              type: typeof message.data,
              nonempty: typeof message.data === "string" && !!message.data,
              legacyFallback: message.data === "fallback-machine-id",
              listener: correlation,
            };
          } else if (message.channel === "PROVIDER_CONNECTIONS") {
            result.providers = {
              data: message.data,
              keys: Object.keys(message.data),
              listener: correlation,
            };
          }
          result.callbacks.push(message.channel);
          if (result.callbacks.length === 2) resolve();
        },
        listener,
      );
      for (const channel of ["DEVICE_ID", "PROVIDER_CONNECTIONS"])
        window.api.send(
          "MAIN",
          {
            channel,
            data: {
              ignored: "payload cannot select a global store",
              machineId: "untrusted",
              contentProviders: { fake: true },
            },
          },
          ids[channel],
        );
    });
    result.current = window.api.getRuntimeContext();
    return result;
  } finally {
    clearTimeout(timer);
    window.api.removeListener("MAIN", listener);
  }
})();
