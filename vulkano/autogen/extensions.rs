// Copyright (c) 2021 The Vulkano developers
// Licensed under the Apache License, Version 2.0
// <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT
// license <LICENSE-MIT or https://opensource.org/licenses/MIT>,
// at your option. All files in the project carrying such
// notice may not be copied, modified, or distributed except
// according to those terms.

use super::{write_file, IndexMap, VkRegistryData};
use heck::ToSnakeCase;
use proc_macro2::{Ident, Literal, TokenStream};
use quote::{format_ident, quote};
use std::{cmp::Ordering, fmt::Write as _};
use vk_parse::Extension;

// This is not included in vk.xml, so it's added here manually
fn required_if_supported(name: &str) -> bool {
    #[allow(clippy::match_like_matches_macro)]
    match name {
        "VK_KHR_portability_subset" => true,
        _ => false,
    }
}

fn conflicts_extensions(name: &str) -> &'static [&'static str] {
    match name {
        "VK_KHR_buffer_device_address" => &["VK_EXT_buffer_device_address"],
        "VK_EXT_buffer_device_address" => &["VK_KHR_buffer_device_address"],
        _ => &[],
    }
}

pub fn write(vk_data: &VkRegistryData) {
    write_device_extensions(vk_data);
    write_instance_extensions(vk_data);
}

#[derive(Clone, Debug)]
struct ExtensionsMember {
    name: Ident,
    doc: String,
    raw: String,
    required_if_supported: bool,
    requires_all_of: Vec<RequiresOneOf>,
    conflicts_device_extensions: Vec<Ident>,
    status: Option<ExtensionStatus>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequiresOneOf {
    pub api_version: Option<(u32, u32)>,
    pub device_extensions: Vec<String>,
    pub instance_extensions: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct RequiresAllOf(pub Vec<Requires>);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Requires {
    APIVersion(u32, u32),
    DeviceExtension(String),
    InstanceExtension(String),
}

impl PartialOrd for Requires {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Requires {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (
                Requires::APIVersion(self_major, self_minor),
                Requires::APIVersion(other_major, other_minor),
            ) => self_major
                .cmp(other_major)
                .then_with(|| self_minor.cmp(other_minor))
                .reverse(),
            (Requires::DeviceExtension(self_ext), Requires::DeviceExtension(other_ext)) => {
                self_ext.cmp(other_ext)
            }
            (Requires::InstanceExtension(self_ext), Requires::InstanceExtension(other_ext)) => {
                self_ext.cmp(other_ext)
            }
            (Requires::APIVersion(_, _), Requires::DeviceExtension(_))
            | (Requires::APIVersion(_, _), Requires::InstanceExtension(_))
            | (Requires::DeviceExtension(_), Requires::InstanceExtension(_)) => Ordering::Less,
            (Requires::DeviceExtension(_), Requires::APIVersion(_, _))
            | (Requires::InstanceExtension(_), Requires::APIVersion(_, _))
            | (Requires::InstanceExtension(_), Requires::DeviceExtension(_)) => Ordering::Greater,
        }
    }
}

#[derive(Clone, Debug)]
enum ExtensionStatus {
    PromotedTo(Requires),
    DeprecatedBy(Option<Requires>),
}

fn write_device_extensions(vk_data: &VkRegistryData) {
    write_file(
        "device_extensions.rs",
        format!(
            "vk.xml header version {}.{}.{}",
            vk_data.header_version.0, vk_data.header_version.1, vk_data.header_version.2
        ),
        device_extensions_output(&extensions_members("device", &vk_data.extensions)),
    );
}

fn write_instance_extensions(vk_data: &VkRegistryData) {
    write_file(
        "instance_extensions.rs",
        format!(
            "vk.xml header version {}.{}.{}",
            vk_data.header_version.0, vk_data.header_version.1, vk_data.header_version.2
        ),
        instance_extensions_output(&extensions_members("instance", &vk_data.extensions)),
    );
}

fn device_extensions_output(members: &[ExtensionsMember]) -> TokenStream {
    let common = extensions_common_output(format_ident!("DeviceExtensions"), members);

    let check_requirements_items = members.iter().map(
        |ExtensionsMember {
             name,
             requires_all_of,
             ..
         }| {
            let name_string = name.to_string();

            let dependency_check_items = requires_all_of.iter().filter_map(
                |RequiresOneOf {
                     api_version,
                     device_extensions,
                     instance_extensions,
                 }| {
                    (device_extensions.is_empty()
                        && (api_version.is_some() || !instance_extensions.is_empty()))
                    .then(|| {
                        let condition_items = (api_version.iter().map(|version| {
                            let version = format_ident!("V{}_{}", version.0, version.1);
                            quote! { api_version >= crate::Version::#version }
                        }))
                        .chain(instance_extensions.iter().map(|ext_name| {
                            let ident = format_ident!("{}", ext_name);
                            quote! { instance_extensions.#ident }
                        }));
                        let requires_one_of_items = (api_version.iter().map(|(major, minor)| {
                            let version = format_ident!("V{}_{}", major, minor);
                            quote! {
                                crate::RequiresAllOf(&[
                                    crate::Requires::APIVersion(crate::Version::#version),
                                ]),
                            }
                        }))
                        .chain(instance_extensions.iter().map(|ext_name| {
                            quote! {
                                crate::RequiresAllOf(&[
                                    crate::Requires::InstanceExtension(#ext_name),
                                ]),
                            }
                        }));
                        let problem = format!("contains `{}`", name_string);

                        quote! {
                            if !(#(#condition_items)||*) {
                                return Err(crate::ValidationError {
                                    problem: #problem.into(),
                                    requires_one_of: crate::RequiresOneOf(&[
                                        #(#requires_one_of_items)*
                                    ]),
                                    ..Default::default()
                                });
                            }
                        }
                    })
                },
            );
            let problem = format!(
                "contains `{}`, but this extension is not supported by the physical device",
                name_string,
            );

            quote! {
                if self.#name {
                    if !supported.#name {
                        return Err(crate::ValidationError {
                            problem: #problem.into(),
                            ..Default::default()
                        });
                    }

                    #(#dependency_check_items)*
                }
            }
        },
    );

    let enable_dependencies_items = members.iter().filter_map(
        |ExtensionsMember {
             name,
             requires_all_of,
             ..
         }| {
            (!requires_all_of.is_empty()).then(|| {
                let requires_all_of_items = requires_all_of.iter().filter_map(
                    |RequiresOneOf {
                         api_version,
                         device_extensions,
                         instance_extensions: _,
                     }| {
                        (!device_extensions.is_empty()).then(|| {
                            let condition_items = api_version
                                .iter()
                                .map(|(major, minor)| {
                                    let version = format_ident!("V{}_{}", major, minor);
                                    quote! { api_version >= crate::Version::#version }
                                })
                                .chain(device_extensions.iter().map(|ext_name| {
                                    let ident = format_ident!("{}", ext_name);
                                    quote! { self.#ident }
                                }));

                            let (base_requirement, promoted_requirements) =
                                device_extensions.split_last().unwrap();

                            let base_requirement_item = {
                                let ident = format_ident!("{}", base_requirement);
                                quote! {
                                    self.#ident = true;
                                }
                            };

                            if promoted_requirements.is_empty() {
                                quote! {
                                    if !(#(#condition_items)||*) {
                                        #base_requirement_item
                                    }
                                }
                            } else {
                                let promoted_requirement_items =
                                    promoted_requirements.iter().map(|name| {
                                        let ident = format_ident!("{}", name);
                                        quote! {
                                            if supported.#ident {
                                                self.#ident = true;
                                            }
                                        }
                                    });

                                quote! {
                                    if !(#(#condition_items)||*) {
                                        #(#promoted_requirement_items)else*
                                        else {
                                            #base_requirement_item
                                        }
                                    }
                                }
                            }
                        })
                    },
                );

                quote! {
                    if self.#name {
                        #(#requires_all_of_items)*
                    }
                }
            })
        },
    );

    quote! {
        #common

        impl DeviceExtensions {
            /// Checks enabled extensions against the physical device support,
            /// and checks for required API version and instance extensions.
            pub(super) fn check_requirements(
                &self,
                supported: &DeviceExtensions,
                api_version: crate::Version,
                instance_extensions: &crate::instance::InstanceExtensions,
            ) -> Result<(), crate::ValidationError> {
                #(#check_requirements_items)*
                Ok(())
            }

            /// Enables all the extensions that the extensions in `self` currently depend on.
            pub(super) fn enable_dependencies(
                &mut self,
                api_version: crate::Version,
                supported: &DeviceExtensions
            ) {
                #(#enable_dependencies_items)*
            }
        }
    }
}

fn instance_extensions_output(members: &[ExtensionsMember]) -> TokenStream {
    let common = extensions_common_output(format_ident!("InstanceExtensions"), members);

    let check_requirements_items = members.iter().map(
        |ExtensionsMember {
             name,
             requires_all_of,
             ..
         }| {
            let name_string = name.to_string();

            let dependency_check_items = requires_all_of.iter().filter_map(
                |RequiresOneOf {
                     api_version,
                     device_extensions: _,
                     instance_extensions,
                 }| {
                    api_version.filter(|_| instance_extensions.is_empty()).map(|(major, minor)| {
                        let version = format_ident!("V{}_{}", major, minor);
                        let problem = format!("contains `{}`", name_string);

                        quote! {
                            if !(api_version >= crate::Version::#version) {
                                return Err(crate::ValidationError {
                                    problem: #problem.into(),
                                    requires_one_of: crate::RequiresOneOf(&[
                                        crate::RequiresAllOf(&[
                                            crate::Requires::APIVersion(crate::Version::#version),
                                        ]),
                                    ]),
                                    ..Default::default()
                                });
                            }
                        }
                    })
                },
            );
            let problem = format!(
                "contains `{}`, but this extension is not supported by the library",
                name_string,
            );

            quote! {
                if self.#name {
                    if !supported.#name {
                        return Err(crate::ValidationError {
                            problem: #problem.into(),
                            ..Default::default()
                        });
                    }

                    #(#dependency_check_items)*
                }
            }
        },
    );

    let enable_dependencies_items = members.iter().filter_map(
        |ExtensionsMember {
             name,
             requires_all_of,
             ..
         }| {
            (!requires_all_of.is_empty()).then(|| {
                let requires_all_of_items = requires_all_of.iter().filter_map(
                    |RequiresOneOf {
                         api_version,
                         device_extensions: _,
                         instance_extensions,
                     }| {
                        (!instance_extensions.is_empty()).then(|| {
                            let condition_items = api_version
                                .iter()
                                .map(|(major, minor)| {
                                    let version = format_ident!("V{}_{}", major, minor);
                                    quote! { api_version >= crate::Version::#version }
                                })
                                .chain(instance_extensions.iter().map(|ext_name| {
                                    let ident = format_ident!("{}", ext_name);
                                    quote! { self.#ident }
                                }));

                            let (base_requirement, promoted_requirements) =
                                instance_extensions.split_last().unwrap();

                            let base_requirement_item = {
                                let ident = format_ident!("{}", base_requirement);
                                quote! {
                                    self.#ident = true;
                                }
                            };

                            if promoted_requirements.is_empty() {
                                quote! {
                                    if !(#(#condition_items)||*) {
                                        #base_requirement_item
                                    }
                                }
                            } else {
                                let promoted_requirement_items =
                                    promoted_requirements.iter().map(|name| {
                                        let ident = format_ident!("{}", name);
                                        quote! {
                                            if supported.#ident {
                                                self.#ident = true;
                                            }
                                        }
                                    });

                                quote! {
                                    if !(#(#condition_items)||*) {
                                        #(#promoted_requirement_items)else*
                                        else {
                                            #base_requirement_item
                                        }
                                    }
                                }
                            }
                        })
                    },
                );

                quote! {
                    if self.#name {
                        #(#requires_all_of_items)*
                    }
                }
            })
        },
    );

    quote! {
        #common

        impl InstanceExtensions {
            /// Checks enabled extensions against the library support,
            /// and checks for required API version.
            pub(super) fn check_requirements(
                &self,
                supported: &InstanceExtensions,
                api_version: crate::Version,
            ) -> Result<(), crate::ValidationError> {
                #(#check_requirements_items)*
                Ok(())
            }

            /// Enables all the extensions that the extensions in `self` currently depend on.
            pub(super) fn enable_dependencies(
                &mut self,
                #[allow(unused_variables)] api_version: crate::Version,
                #[allow(unused_variables)]supported: &InstanceExtensions
            ) {
                #(#enable_dependencies_items)*
            }
        }
    }
}

fn extensions_common_output(struct_name: Ident, members: &[ExtensionsMember]) -> TokenStream {
    let struct_items = members.iter().map(|ExtensionsMember { name, doc, .. }| {
        quote! {
            #[doc = #doc]
            pub #name: bool,
        }
    });

    let empty_items = members.iter().map(|ExtensionsMember { name, .. }| {
        quote! {
            #name: false,
        }
    });

    let intersects_items = members.iter().map(|ExtensionsMember { name, .. }| {
        quote! {
            (self.#name && other.#name)
        }
    });

    let contains_items = members.iter().map(|ExtensionsMember { name, .. }| {
        quote! {
            (self.#name || !other.#name)
        }
    });

    let union_items = members.iter().map(|ExtensionsMember { name, .. }| {
        quote! {
            #name: self.#name || other.#name,
        }
    });

    let intersection_items = members.iter().map(|ExtensionsMember { name, .. }| {
        quote! {
            #name: self.#name && other.#name,
        }
    });

    let difference_items = members.iter().map(|ExtensionsMember { name, .. }| {
        quote! {
            #name: self.#name && !other.#name,
        }
    });

    let symmetric_difference_items = members.iter().map(|ExtensionsMember { name, .. }| {
        quote! {
            #name: self.#name ^ other.#name,
        }
    });

    let debug_items = members.iter().map(|ExtensionsMember { name, raw, .. }| {
        quote! {
            if self.#name {
                if !first { write!(f, ", ")? }
                else { first = false; }
                f.write_str(#raw)?;
            }
        }
    });

    let arr_items = members.iter().map(|ExtensionsMember { name, raw, .. }| {
        quote! {
            (#raw, self.#name),
        }
    });
    let arr_len = members.len();

    let from_str_for_extensions_items =
        members.iter().map(|ExtensionsMember { name, raw, .. }| {
            let raw = Literal::string(raw);
            quote! {
                #raw => { extensions.#name = true; }
            }
        });

    let from_extensions_for_vec_cstring_items =
        members.iter().map(|ExtensionsMember { name, raw, .. }| {
            quote! {
                if x.#name { data.push(std::ffi::CString::new(#raw).unwrap()); }
            }
        });

    quote! {
        /// List of extensions that are enabled or available.
        #[derive(Copy, Clone, PartialEq, Eq)]
        pub struct #struct_name {
            #(#struct_items)*

            pub _ne: crate::NonExhaustive,
        }

        impl Default for #struct_name {
            #[inline]
            fn default() -> Self {
                Self::empty()
            }
        }

        impl #struct_name {
            /// Returns an `Extensions` object with none of the members set.
            #[inline]
            pub const fn empty() -> Self {
                Self {
                    #(#empty_items)*
                    _ne: crate::NonExhaustive(()),
                }
            }

            /// Returns an `Extensions` object with none of the members set.
            #[deprecated(since = "0.31.0", note = "Use `empty` instead.")]
            #[inline]
            pub const fn none() -> Self {
                Self::empty()
            }

            /// Returns whether any members are set in both `self` and `other`.
            #[inline]
            pub const fn intersects(&self, other: &Self) -> bool {
                #(#intersects_items)||*
            }

            /// Returns whether all members in `other` are set in `self`.
            #[inline]
            pub const fn contains(&self, other: &Self) -> bool {
                #(#contains_items)&&*
            }

            /// Returns whether all members in `other` are set in `self`.
            #[deprecated(since = "0.31.0", note = "Use `contains` instead.")]
            #[inline]
            pub const fn is_superset_of(&self, other: &Self) -> bool {
                self.contains(other)
            }

            /// Returns the union of `self` and `other`.
            #[inline]
            pub const fn union(&self, other: &Self) -> Self {
                Self {
                    #(#union_items)*
                    _ne: crate::NonExhaustive(()),
                }
            }

            /// Returns the intersection of `self` and `other`.
            #[inline]
            pub const fn intersection(&self, other: &Self) -> Self {
                Self {
                    #(#intersection_items)*
                    _ne: crate::NonExhaustive(()),
                }
            }

            /// Returns `self` without the members set in `other`.
            #[inline]
            pub const fn difference(&self, other: &Self) -> Self {
                Self {
                    #(#difference_items)*
                    _ne: crate::NonExhaustive(()),
                }
            }

            /// Returns the members set in `self` or `other`, but not both.
            #[inline]
            pub const fn symmetric_difference(&self, other: &Self) -> Self {
                Self {
                    #(#symmetric_difference_items)*
                    _ne: crate::NonExhaustive(()),
                }
            }
        }

        impl std::ops::BitAnd for #struct_name {
            type Output = #struct_name;

            #[inline]
            fn bitand(self, rhs: Self) -> Self::Output {
                self.union(&rhs)
            }
        }

        impl std::ops::BitAndAssign for #struct_name {
            #[inline]
            fn bitand_assign(&mut self, rhs: Self) {
                *self = self.union(&rhs);
            }
        }

        impl std::ops::BitOr for #struct_name {
            type Output = #struct_name;

            #[inline]
            fn bitor(self, rhs: Self) -> Self::Output {
                self.intersection(&rhs)
            }
        }

        impl std::ops::BitOrAssign for #struct_name {
            #[inline]
            fn bitor_assign(&mut self, rhs: Self) {
                *self = self.intersection(&rhs);
            }
        }

        impl std::ops::BitXor for #struct_name {
            type Output = #struct_name;

            #[inline]
            fn bitxor(self, rhs: Self) -> Self::Output {
                self.symmetric_difference(&rhs)
            }
        }

        impl std::ops::BitXorAssign for #struct_name {
            #[inline]
            fn bitxor_assign(&mut self, rhs: Self) {
                *self = self.symmetric_difference(&rhs);
            }
        }

        impl std::ops::Sub for #struct_name {
            type Output = #struct_name;

            #[inline]
            fn sub(self, rhs: Self) -> Self::Output {
                self.difference(&rhs)
            }
        }

        impl std::ops::SubAssign for #struct_name {
            #[inline]
            fn sub_assign(&mut self, rhs: Self) {
                *self = self.difference(&rhs);
            }
        }

        impl std::fmt::Debug for #struct_name {
            #[allow(unused_assignments)]
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
                write!(f, "[")?;

                let mut first = true;
                #(#debug_items)*

                write!(f, "]")
            }
        }

        impl<'a> FromIterator<&'a str> for #struct_name {
            fn from_iter<I>(iter: I) -> Self
                where I: IntoIterator<Item = &'a str>
            {
                let mut extensions = Self::empty();
                for name in iter {
                    match name {
                        #(#from_str_for_extensions_items)*
                        _ => (),
                    }
                }
                extensions
            }
        }

        impl<'a> From<&'a #struct_name> for Vec<std::ffi::CString> {
            fn from(x: &'a #struct_name) -> Self {
                let mut data = Self::new();
                #(#from_extensions_for_vec_cstring_items)*
                data
            }
        }

        impl IntoIterator for #struct_name {
            type Item = (&'static str, bool);
            type IntoIter = std::array::IntoIter<Self::Item, #arr_len>;

            #[inline]
            fn into_iter(self) -> Self::IntoIter {
                [#(#arr_items)*].into_iter()
            }
        }
    }
}

fn extensions_members(ty: &str, extensions: &IndexMap<&str, &Extension>) -> Vec<ExtensionsMember> {
    extensions
        .values()
        .filter(|ext| ext.ext_type.as_ref().unwrap() == ty)
        .map(|ext| {
            let raw = ext.name.to_owned();
            let name = raw.strip_prefix("VK_").unwrap().to_snake_case();

            let requires_all_of = {
                let mut requires_all_of = Vec::new();

                if let Some(core) = ext.requires_core.as_ref() {
                    let (major, minor) = core.split_once('.').unwrap();
                    requires_all_of.push(RequiresOneOf {
                        api_version: Some((major.parse().unwrap(), minor.parse().unwrap())),
                        ..Default::default()
                    });
                }

                if let Some(req) = ext.requires.as_ref() {
                    requires_all_of.extend(req.split(',').map(|mut vk_name| {
                        let mut requires_one_of = RequiresOneOf::default();

                        loop {
                            if let Some(version) = vk_name.strip_prefix("VK_VERSION_") {
                                let (major, minor) = version.split_once('_').unwrap();
                                requires_one_of.api_version =
                                    Some((major.parse().unwrap(), minor.parse().unwrap()));
                                break;
                            } else {
                                let ext_name = vk_name.strip_prefix("VK_").unwrap().to_snake_case();
                                let extension = extensions[vk_name];

                                match extension.ext_type.as_deref() {
                                    Some("device") => &mut requires_one_of.device_extensions,
                                    Some("instance") => &mut requires_one_of.instance_extensions,
                                    _ => unreachable!(),
                                }
                                .push(ext_name);

                                if let Some(promotedto) = extension.promotedto.as_ref() {
                                    vk_name = promotedto.as_str();
                                } else {
                                    break;
                                }
                            }
                        }

                        requires_one_of.device_extensions.reverse();
                        requires_one_of.instance_extensions.reverse();
                        requires_one_of
                    }));
                }

                requires_all_of
            };

            let conflicts_extensions = conflicts_extensions(&ext.name);

            let mut member = ExtensionsMember {
                name: format_ident!("{}", name),
                doc: String::new(),
                raw,
                required_if_supported: required_if_supported(ext.name.as_str()),
                requires_all_of,
                conflicts_device_extensions: conflicts_extensions
                    .iter()
                    .filter(|&&vk_name| extensions[vk_name].ext_type.as_ref().unwrap() == "device")
                    .map(|vk_name| {
                        format_ident!("{}", vk_name.strip_prefix("VK_").unwrap().to_snake_case())
                    })
                    .collect(),
                status: ext
                    .promotedto
                    .as_deref()
                    .and_then(|pr| {
                        if let Some(version) = pr.strip_prefix("VK_VERSION_") {
                            let (major, minor) = version.split_once('_').unwrap();
                            Some(ExtensionStatus::PromotedTo(Requires::APIVersion(
                                major.parse().unwrap(),
                                minor.parse().unwrap(),
                            )))
                        } else {
                            let ext_name = pr.strip_prefix("VK_").unwrap().to_snake_case();
                            match extensions[pr].ext_type.as_ref().unwrap().as_str() {
                                "device" => Some(ExtensionStatus::PromotedTo(
                                    Requires::DeviceExtension(ext_name),
                                )),
                                "instance" => Some(ExtensionStatus::PromotedTo(
                                    Requires::InstanceExtension(ext_name),
                                )),
                                _ => unreachable!(),
                            }
                        }
                    })
                    .or_else(|| {
                        ext.deprecatedby.as_deref().and_then(|depr| {
                            if depr.is_empty() {
                                Some(ExtensionStatus::DeprecatedBy(None))
                            } else if let Some(version) = depr.strip_prefix("VK_VERSION_") {
                                let (major, minor) = version.split_once('_').unwrap();
                                Some(ExtensionStatus::DeprecatedBy(Some(Requires::APIVersion(
                                    major.parse().unwrap(),
                                    minor.parse().unwrap(),
                                ))))
                            } else {
                                let ext_name = depr.strip_prefix("VK_").unwrap().to_snake_case();
                                match extensions[depr].ext_type.as_ref().unwrap().as_str() {
                                    "device" => Some(ExtensionStatus::DeprecatedBy(Some(
                                        Requires::DeviceExtension(ext_name),
                                    ))),
                                    "instance" => Some(ExtensionStatus::DeprecatedBy(Some(
                                        Requires::InstanceExtension(ext_name),
                                    ))),
                                    _ => unreachable!(),
                                }
                            }
                        })
                    }),
            };
            make_doc(&mut member);
            member
        })
        .collect()
}

fn make_doc(ext: &mut ExtensionsMember) {
    let writer = &mut ext.doc;
    write!(writer, "- [Vulkan documentation](https://registry.khronos.org/vulkan/specs/1.3-extensions/man/html/{}.html)", ext.raw).unwrap();

    if ext.required_if_supported {
        write!(
            writer,
            "\n- Must be enabled if it is supported by the physical device",
        )
        .unwrap();
    }

    if let Some(status) = ext.status.as_ref() {
        match status {
            ExtensionStatus::PromotedTo(replacement) => {
                write!(writer, "\n- Promoted to ",).unwrap();

                match replacement {
                    Requires::APIVersion(major, minor) => {
                        write!(writer, "Vulkan {}.{}", major, minor).unwrap();
                    }
                    Requires::DeviceExtension(ext_name) => {
                        write!(
                            writer,
                            "[`{}`](crate::device::DeviceExtensions::{0})",
                            ext_name
                        )
                        .unwrap();
                    }
                    Requires::InstanceExtension(ext_name) => {
                        write!(
                            writer,
                            "[`{}`](crate::instance::InstanceExtensions::{0})",
                            ext_name
                        )
                        .unwrap();
                    }
                }
            }
            ExtensionStatus::DeprecatedBy(replacement) => {
                write!(writer, "\n- Deprecated ",).unwrap();

                match replacement {
                    Some(Requires::APIVersion(major, minor)) => {
                        write!(writer, "by Vulkan {}.{}", major, minor).unwrap();
                    }
                    Some(Requires::DeviceExtension(ext_name)) => {
                        write!(
                            writer,
                            "by [`{}`](crate::device::DeviceExtensions::{0})",
                            ext_name
                        )
                        .unwrap();
                    }
                    Some(Requires::InstanceExtension(ext_name)) => {
                        write!(
                            writer,
                            "by [`{}`](crate::instance::InstanceExtensions::{0})",
                            ext_name
                        )
                        .unwrap();
                    }
                    None => {
                        write!(writer, "without a replacement").unwrap();
                    }
                }
            }
        }
    }

    if !ext.requires_all_of.is_empty() {
        write!(writer, "\n- Requires all of:").unwrap();
    }

    for require in &ext.requires_all_of {
        let mut line = Vec::new();

        if let Some((major, minor)) = require.api_version.as_ref() {
            line.push(format!("Vulkan API version {}.{}", major, minor));
        }

        line.extend(require.device_extensions.iter().map(|ext| {
            format!(
                "device extension [`{}`](crate::device::DeviceExtensions::{0})",
                ext
            )
        }));
        line.extend(require.instance_extensions.iter().map(|ext| {
            format!(
                "instance extension [`{}`](crate::instance::InstanceExtensions::{0})",
                ext
            )
        }));

        write!(writer, "\n  - {}", line.join(" or ")).unwrap();
    }

    if !ext.conflicts_device_extensions.is_empty() {
        let links: Vec<_> = ext
            .conflicts_device_extensions
            .iter()
            .map(|ext| format!("[`{}`](crate::device::DeviceExtensions::{0})", ext))
            .collect();
        write!(
            writer,
            "\n- Conflicts with device extension{}: {}",
            if ext.conflicts_device_extensions.len() > 1 {
                "s"
            } else {
                ""
            },
            links.join(", ")
        )
        .unwrap();
    }
}
