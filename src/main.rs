mod audio;
mod input;
mod tui;

use std::io::Write;

use color_eyre::Result;
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    FromSample, SizedSample,
};

fn main() -> Result<()> {
    color_eyre::install()?;

    let (input_tui_tx, input_tui_rx) = std::sync::mpsc::channel::<tui::Cmd>();
    let (input_pads_tx, input_pads_rx) = std::sync::mpsc::channel::<audio::Cmd>();

    let hosts = cpal::available_hosts();
    let id = match hosts.len() {
        0 => return Err(color_eyre::Report::msg("no audio host found")),
        1 => {
            println!("selected only available audio host: {}", hosts[0].name(),);
            hosts[0]
        }
        _ => {
            println!("available audio hosts:");
            for (i, h) in hosts.iter().enumerate() {
                println!("{}: {}", i, h.name())
            }
            print!("select an audio host: ");
            std::io::stdout().flush()?;
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            *hosts
                .get(input.trim().parse::<usize>()?)
                .ok_or(color_eyre::Report::msg("invalid audio host selected"))?
        }
    };
    let host = cpal::host_from_id(id)?;

    let devices = host.output_devices().into_iter().flatten().collect::<Vec<_>>();
    let device = match devices.len() {
        0 => return Err(color_eyre::Report::msg("no audio device found")),
        1 => {
            println!(
                "\nselected only available audio device: {}",
                devices[0].name()?,
            );
            devices[0].clone()
        }
        _ => {
            println!("\navailable audio devices:");
            for (i, d) in devices.iter().enumerate() {
                println!("{}: {}", i, d.name()?)
            }
            print!("select an audio device: ");
            std::io::stdout().flush()?;
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            devices
                .get(input.trim().parse::<usize>()?)
                .ok_or(color_eyre::Report::msg("invalid audio device selected"))?
                .clone()
        }
    };

    let midi_in = midir::MidiInput::new("angry-surgeon")?;
    let in_ports = midi_in.ports();
    let in_port = match in_ports.len() {
        0 => return Err(color_eyre::Report::msg("no midi input port found")),
        1 => {
            println!(
                "\nselected only available input port: {}",
                midi_in.port_name(&in_ports[0]).unwrap()
            );
            &in_ports[0]
        }
        _ => {
            println!("\navailable input ports:");
            for (i, p) in in_ports.iter().enumerate() {
                println!("{}: {}", i, midi_in.port_name(p).unwrap());
            }
            print!("select an input port: ");
            std::io::stdout().flush()?;
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            in_ports
                .get(input.trim().parse::<usize>()?)
                .ok_or(color_eyre::Report::msg("invalid input port selected"))?
        }
    };
    let input_handler = input::InputHandler::new(input_tui_tx, input_pads_tx)?;
    let midi_in = midi_in
        .connect(
            in_port,
            "angry-surgeon",
            move |_, message, input_handler: &mut input::InputHandler| {
                input_handler.push(message).unwrap();
            },
            input_handler,
        )
        .map_err(|_| color_eyre::Report::msg("failed to connect midi input"))?;

    println!("\nplease make some noise <3");
    std::thread::sleep(std::time::Duration::from_millis(1000));

    let audio_handle = std::thread::spawn(move || -> Result<()> {
        let config = device.default_output_config().unwrap();
        let pads = audio::pads::PadsHandler::<{audio::PAD_COUNT}>::new(input_pads_rx);

        match config.sample_format() {
            cpal::SampleFormat::I16 => play::<i16>(&device, &config.into(), pads)?,
            cpal::SampleFormat::F32 => play::<f32>(&device, &config.into(), pads)?,
            sample_format => panic!("unsupported sample format: {}", sample_format),
        }
        Ok(())
    });

    let mut terminal = ratatui::init();
    tui::Tui::default().run(&mut terminal, input_tui_rx)?;
    ratatui::restore();

    // pads thread completes once audio_tx held by input_handler dropped in _in_connection thread
    std::mem::drop(midi_in);

    audio_handle.thread().unpark();
    audio_handle.join().unwrap()?;

    Ok(())
}

fn play<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut pads: audio::pads::PadsHandler<{audio::PAD_COUNT}>,
) -> Result<()>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;

    let out_fn = move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
        pads.tick(data, channels).unwrap();
    };
    let err_fn = |err| eprintln!("error occurred on stream: {}", err);
    let stream = device.build_output_stream(config, out_fn, err_fn, None)?;

    stream.play()?;
    std::thread::park();

    Ok(())
}
