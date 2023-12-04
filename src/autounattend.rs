// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Functions for updating a template Autounattend.xml inline to support
//! selecting specific Windows editions and virtio driver versions.

use std::{
    fs::File,
    io::{BufReader, BufWriter},
    path::Path,
};

use anyhow::{Context, Result};

#[derive(Clone, Copy, Debug)]
pub enum VirtioDriverVersion {
    Server2016,
    Server2019,
    Server2022,
}

// The general idea here is to stream in elements from an Autounattend.xml
// looking for a hierarchy of tags that matches something the caller wants to
// replace. For editions, this is buried in the Microsoft-Windows-Setup
// component:
//
// <unattend>
//   <settings pass="windowsPE">
//     <component name="Microsoft-Windows-Setup">
//       <ImageInstall>
//         <OSImage>
//           <InstallFrom>
//             <MetaData wcm:action="add">
//               <Key>/IMAGE/INDEX</Key>
//               <Value>2</Value> <-- replace this
//
// For drivers the component of interest is
// Microsoft-Windows-PnpCustomizationsNonWinPE:
//
// <unattend>
//   <settings pass="offlineServicing">
//     <component name="Microsoft-Windows-PnpCustomizationsNonWinPE">
//       <DriverPaths>
//         <PathAndCredentials wcm:action="add" wcm:keyValue="1">
//           <Path>D:\NetKVM\2k22\amd64</Path> <-- substitute for 2k22
//         </PathAndCredentials>
//         <PathAndCredentials wcm:action="add" wcm:keyValue="2">
//           <Path>D:\viostor\2k22\amd64</Path>
//         </PathAndCredentials>
//         <!-- etc -->
//
// Note that only the Linux version of the tool uses the latter construction;
// the illumos scripts copy appropriately versioned drivers out of the driver
// ISO and put them into the installer disk directly, so it needs to substitute
// paths when deciding where to copy from instead of changing the answer file.

/// An attribute within the start of an element that must be present in order
/// for the element to match a rule.
struct MatchAttribute {
    name: &'static str,
    value: &'static str,
}

/// A matching rule for the start of an element.
struct MatchElement {
    name: &'static str,
    attributes: Vec<MatchAttribute>,
}

/// A replacement rule, specifying a set of elements that, if matched, should
/// result in the contents of the last element being replaced using the method
/// in the rule's `replace_fn`.
struct ReplacementRule {
    elements: Vec<MatchElement>,
    replace_fn: Box<dyn Fn(&str) -> String>,
}

impl ReplacementRule {
    fn is_armed(&self, depth: usize) -> bool {
        depth == self.elements.len()
    }
}

fn replace_image_index(_: &str, new_index: u32) -> String {
    new_index.to_string()
}

fn replace_version_in_driver_path(
    path: &str,
    version: VirtioDriverVersion,
) -> String {
    path.replace(
        "\\2k22\\",
        match version {
            VirtioDriverVersion::Server2016 => "\\2k16\\",
            VirtioDriverVersion::Server2019 => "\\2k19\\",
            VirtioDriverVersion::Server2022 => "\\2k22\\",
        },
    )
}

pub struct AutounattendUpdater {
    rules: Vec<ReplacementRule>,
}

impl AutounattendUpdater {
    pub fn new(
        image_index: Option<u32>,
        virtio_driver_version: Option<VirtioDriverVersion>,
    ) -> Self {
        let mut rules = Vec::new();
        if let Some(index) = image_index {
            let elements = vec![
                MatchElement { name: "unattend", attributes: vec![] },
                MatchElement {
                    name: "settings",
                    attributes: vec![MatchAttribute {
                        name: "pass",
                        value: "windowsPE",
                    }],
                },
                MatchElement {
                    name: "component",
                    attributes: vec![MatchAttribute {
                        name: "name",
                        value: "Microsoft-Windows-Setup",
                    }],
                },
                MatchElement { name: "ImageInstall", attributes: vec![] },
                MatchElement { name: "OSImage", attributes: vec![] },
                MatchElement { name: "InstallFrom", attributes: vec![] },
                MatchElement {
                    name: "MetaData",
                    attributes: vec![MatchAttribute {
                        name: "action",
                        value: "add",
                    }],
                },
                MatchElement { name: "Value", attributes: vec![] },
            ];

            rules.push(ReplacementRule {
                elements,
                replace_fn: Box::new(move |old_index| {
                    replace_image_index(old_index, index)
                }),
            });
        }

        if let Some(version) = virtio_driver_version {
            let elements = vec![
                MatchElement { name: "unattend", attributes: vec![] },
                MatchElement {
                    name: "settings",
                    attributes: vec![MatchAttribute {
                        name: "pass",
                        value: "offlineServicing",
                    }],
                },
                MatchElement {
                    name: "component",
                    attributes: vec![MatchAttribute {
                        name: "name",
                        value: "Microsoft-Windows-PnpCustomizationsNonWinPE",
                    }],
                },
                MatchElement {
                    name: "PathAndCredentials",
                    attributes: vec![MatchAttribute {
                        name: "action",
                        value: "add",
                    }],
                },
                MatchElement { name: "Path", attributes: vec![] },
            ];

            rules.push(ReplacementRule {
                elements,
                replace_fn: Box::new(move |path| {
                    replace_version_in_driver_path(path, version)
                }),
            });
        }

        Self { rules }
    }

    pub fn run(
        &self,
        input: impl AsRef<Path>,
        output: impl AsRef<Path>,
    ) -> Result<usize> {
        let infile = File::open(input)?;
        let reader = xml::EventReader::new(BufReader::new(infile));
        let outfile = File::create(output)?;
        let writer = xml::EventWriter::new(BufWriter::new(outfile));

        self.run_internal(reader, writer)
    }

    fn run_internal<R: std::io::Read, W: std::io::Write>(
        &self,
        input: xml::EventReader<R>,
        mut output: xml::EventWriter<W>,
    ) -> Result<usize> {
        let mut matches = 0;
        let mut next_match_depths = vec![];
        for _ in &self.rules {
            next_match_depths.push(0);
        }

        for e in input {
            if let Ok(e) = &e {
                match e {
                    xml::reader::XmlEvent::StartElement {
                        name,
                        attributes,
                        ..
                    } => {
                        dbg!(&next_match_depths, &name.local_name, attributes);
                        for (rule_index, rule) in self.rules.iter().enumerate()
                        {
                            let depth = &mut next_match_depths[rule_index];
                            let to_match = &rule.elements[*depth];

                            // If this element's local name matches the rule's
                            // name...
                            let names_match = to_match.name == name.local_name;

                            // ...and each attribute in the rule has a matching
                            // attribute in the element...
                            let attributes_match =
                                to_match.attributes.iter().all(|attr| {
                                    attributes.iter().any(|xml_attr| {
                                        attr.name == xml_attr.name.local_name
                                            && attr.value == xml_attr.value
                                    })
                                });

                            // ...this is a match, so move to considering the
                            // next element in this rule.
                            if names_match && attributes_match {
                                *depth += 1;
                            }
                        }
                    }
                    xml::reader::XmlEvent::EndElement { name } => {
                        for (rule_index, rule) in self.rules.iter().enumerate()
                        {
                            let depth = &mut next_match_depths[rule_index];

                            // If this is the end of the most-recently-matched
                            // element in this rule, move back one element.
                            let to_match = &rule.elements[*depth - 1];
                            if to_match.name == name.local_name {
                                *depth -= 1;
                            }
                        }
                    }
                    xml::reader::XmlEvent::Characters(data) => {
                        let mut wrote = false;
                        for (rule_index, rule) in self.rules.iter().enumerate()
                        {
                            if rule.is_armed(next_match_depths[rule_index]) {
                                assert!(!wrote);

                                let new_data = (rule.replace_fn)(data);
                                output.write(
                                    xml::writer::XmlEvent::Characters(
                                        &new_data,
                                    ),
                                )?;

                                wrote = true;
                                matches += 1;
                            }
                        }

                        if wrote {
                            continue;
                        }
                    }
                    _ => {}
                }

                if let Some(writer_event) = e.as_writer_event() {
                    output.write(writer_event)?;
                }
            } else {
                e.context("updating Autounattend.xml")?;
            }
        }

        Ok(matches)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn replace_illumos_unattend() {
        let updater = AutounattendUpdater::new(
            Some(1),
            Some(VirtioDriverVersion::Server2016),
        );

        let original = include_str!("../illumos/unattend/Autounattend.xml");

        let reader = xml::EventReader::new(original.as_bytes());
        let writer = xml::EventWriter::new(std::io::empty());

        assert_eq!(updater.run_internal(reader, writer).unwrap(), 2);
    }

    #[test]
    fn replace_linux_unattend() {
        let updater = AutounattendUpdater::new(
            Some(1),
            Some(VirtioDriverVersion::Server2016),
        );

        let original = include_str!("../linux/unattend/Autounattend.xml");

        let reader = xml::EventReader::new(original.as_bytes());
        let writer = xml::EventWriter::new(std::io::stderr());

        assert_eq!(updater.run_internal(reader, writer).unwrap(), 7);
    }
}
