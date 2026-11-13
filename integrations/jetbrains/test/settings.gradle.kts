import org.jetbrains.intellij.platform.gradle.extensions.intellijPlatform

// The same plugin and repositories as JetBrains' intellij-platform-plugin-template, whose tests pass on this pair.
rootProject.name = "scrutate-lsp4ij-test"

plugins {
    id("org.jetbrains.intellij.platform.settings") version "2.19.0"
}

@Suppress("UnstableApiUsage")
dependencyResolutionManagement {
    repositories {
        mavenCentral()
        intellijPlatform {
            defaultRepositories()
        }
    }
}
